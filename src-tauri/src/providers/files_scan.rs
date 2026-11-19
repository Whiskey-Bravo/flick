//! The files index: a compact, query-friendly layout, and the folder walk that fills it.
//!
//! Names live in two string arenas (as shown, and normalised) and each entry
//! points at its parent folder by number, so a million entries cost a few
//! allocations instead of three per entry, and a query scans contiguous memory.

use std::collections::VecDeque;
use std::os::windows::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::fuzzy;

const FILE_ATTRIBUTE_HIDDEN: u32 = 0x2;
const FILE_ATTRIBUTE_SYSTEM: u32 = 0x4;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x10;

/// Folders never walked (compared without case). Names starting with `.` are
/// skipped everywhere.
/// Package caches, build output and Python distributions (whose `pkgs` and
/// `envs` hold whole copies of Python), whose names would flood the results.
const JUNK_DIRS: [&str; 18] = [
    "node_modules",
    "target",
    "dist",
    "build",
    "__pycache__",
    "venv",
    ".venv",
    "AppData",
    "$RECYCLE.BIN",
    "vendor",
    "site-packages",
    "Pods",
    "bower_components",
    "obj",
    "miniconda3",
    "anaconda3",
    "miniforge3",
    "mambaforge",
];

pub(super) fn skip_by_attributes(attrs: u32) -> bool {
    attrs & (FILE_ATTRIBUTE_HIDDEN | FILE_ATTRIBUTE_SYSTEM) != 0
}

/// Whether two paths name the same folder (no disk access): equal without
/// case, `/` and `\` alike, ignoring a trailing separator.
fn same_folder(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('/', r"\").trim_end_matches('\\').to_ascii_lowercase();
    norm(a) == norm(b)
}

pub(super) fn is_junk_dir(name: &str) -> bool {
    JUNK_DIRS.iter().any(|j| j.eq_ignore_ascii_case(name))
}

/// One bit per char class: `a`-`z`, `0`-`9`, other ASCII folded into 27
/// buckets, everything else in the last bit. A needle can only match a name
/// whose mask holds all of the needle's bits.
pub(super) fn char_bit(c: char) -> u64 {
    let b = match c {
        'a'..='z' => c as u32 - 'a' as u32,
        '0'..='9' => 26 + (c as u32 - '0' as u32),
        c if c.is_ascii() => 36 + (c as u32 % 27),
        _ => 63,
    };
    1 << b
}

pub(super) struct Slot {
    norm: (u32, u32),
    name: (u32, u32),
    pub parent: u32,
    pub is_dir: bool,
    /// [`char_bit`]s of every char of the normalised name except word breaks.
    pub chars: u64,
    /// [`char_bit`]s of the first char of each word of the normalised name.
    pub initials: u64,
}

#[derive(Default)]
pub(super) struct Index {
    /// Folders that entries live in, by number.
    dirs: Vec<PathBuf>,
    /// Levels below its root, parallel to `dirs`: a root is 0, so an entry
    /// directly in a root has depth 1.
    depths: Vec<u8>,
    names: String,
    norms: String,
    pub slots: Vec<Slot>,
}

/// What an index holds, by count: the hint for sizing the next scan.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Sizes {
    pub slots: usize,
    pub names: usize,
    pub norms: usize,
    pub dirs: usize,
}

/// Heap bytes of an index: `used` by its contents, `reserved` by its
/// allocations (capacities), and the bytes of the `norms` arena alone, which
/// duplicates the names in lower case (part of `used`).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(super) struct Memory {
    pub used: usize,
    pub reserved: usize,
    pub norms: usize,
}

/// Room a scan keeps beyond the previous index's size: a small margin so a few
/// new entries do not regrow the vector.
fn with_margin(n: usize) -> usize {
    if n == 0 {
        0
    } else {
        n + n / 50 + 64
    }
}

/// Whether `cap` exceeds `len` by more than 5 %: only then is shrinking worth a reallocation.
fn wasteful(len: usize, cap: usize) -> bool {
    cap > len + len / 20
}

impl Index {
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn sizes(&self) -> Sizes {
        Sizes { slots: self.slots.len(), names: self.names.len(), norms: self.norms.len(), dirs: self.dirs.len() }
    }

    /// Bytes on the heap, from lengths and capacities (no allocation walk).
    pub fn memory(&self) -> Memory {
        use std::mem::size_of;
        let dir_bytes = |cap: bool| -> usize {
            self.dirs.iter().map(|d| if cap { d.capacity() } else { d.as_os_str().len() }).sum()
        };
        let used = self.slots.len() * size_of::<Slot>()
            + self.names.len()
            + self.norms.len()
            + self.dirs.len() * size_of::<PathBuf>()
            + dir_bytes(false)
            + self.depths.len();
        let reserved = self.slots.capacity() * size_of::<Slot>()
            + self.names.capacity()
            + self.norms.capacity()
            + self.dirs.capacity() * size_of::<PathBuf>()
            + dir_bytes(true)
            + self.depths.capacity();
        Memory { used, reserved, norms: self.norms.len() }
    }

    fn with_capacity(hint: Sizes) -> Self {
        Index {
            dirs: Vec::with_capacity(with_margin(hint.dirs)),
            depths: Vec::with_capacity(with_margin(hint.dirs)),
            names: String::with_capacity(with_margin(hint.names)),
            norms: String::with_capacity(with_margin(hint.norms)),
            slots: Vec::with_capacity(with_margin(hint.slots)),
        }
    }

    /// Gives back capacity beyond 5 % of what is used, per allocation.
    fn trim(&mut self) {
        if wasteful(self.slots.len(), self.slots.capacity()) {
            self.slots.shrink_to_fit();
        }
        if wasteful(self.names.len(), self.names.capacity()) {
            self.names.shrink_to_fit();
        }
        if wasteful(self.norms.len(), self.norms.capacity()) {
            self.norms.shrink_to_fit();
        }
        if wasteful(self.depths.len(), self.depths.capacity()) {
            self.depths.shrink_to_fit();
        }
        if wasteful(self.dirs.len(), self.dirs.capacity()) {
            self.dirs.shrink_to_fit();
        }
        let (len, cap) = self.dirs.iter().fold((0, 0), |(l, c), d| (l + d.as_os_str().len(), c + d.capacity()));
        if wasteful(len, cap) {
            for d in &mut self.dirs {
                d.shrink_to_fit();
            }
        }
    }

    pub fn name(&self, s: &Slot) -> &str {
        &self.names[s.name.0 as usize..s.name.1 as usize]
    }

    pub fn norm(&self, s: &Slot) -> &str {
        &self.norms[s.norm.0 as usize..s.norm.1 as usize]
    }

    pub fn dir(&self, s: &Slot) -> &Path {
        &self.dirs[s.parent as usize]
    }

    /// Levels below its root: 1 for an entry directly in a root.
    pub fn depth(&self, s: &Slot) -> u8 {
        self.depths[s.parent as usize].saturating_add(1)
    }

    pub fn path(&self, s: &Slot) -> PathBuf {
        self.dir(s).join(self.name(s))
    }

    /// The entry whose full path is `path`, compared exactly.
    pub fn find(&self, path: &Path) -> Option<&Slot> {
        let (Some(dir), Some(name)) = (path.parent(), path.file_name().and_then(|n| n.to_str())) else {
            return None;
        };
        self.slots.iter().find(|s| self.name(s) == name && self.dir(s) == dir)
    }

    /// A copy of the index after `old` (and what is below it) moved to `new`,
    /// or, with `None`, was removed. Built beside the live one: the caller
    /// swaps it in. Names are compared without regard to case.
    pub fn patched(&self, old: &Path, new: Option<&Path>) -> Index {
        let key = |p: &Path| p.to_string_lossy().replace('/', r"\").trim_end_matches('\\').to_lowercase();
        let old_key = key(old);
        // Whether a folder is `old` or below it, and where it goes.
        let below = |dir: &Path| {
            let k = key(dir);
            k == old_key || k.strip_prefix(&old_key).is_some_and(|rest| rest.starts_with('\\'))
        };
        let mut out = Index::with_capacity(self.sizes());
        out.dirs = self
            .dirs
            .iter()
            .map(|d| match new {
                Some(new) if below(d) => {
                    let rest = d.to_string_lossy().get(old.to_string_lossy().trim_end_matches('\\').len()..).unwrap_or("").to_string();
                    PathBuf::from(format!("{}{rest}", new.to_string_lossy().trim_end_matches('\\')))
                }
                _ => d.clone(),
            })
            .collect();
        out.depths = self.depths.clone();
        let gone: Vec<bool> = self.dirs.iter().map(|d| new.is_none() && below(d)).collect();
        let old_parent = old.parent().map(key);
        let old_name = old.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
        let beside: Vec<bool> = self.dirs.iter().map(|d| Some(key(d)) == old_parent).collect();
        for s in &self.slots {
            let is_old = beside[s.parent as usize] && self.name(s).to_lowercase() == old_name;
            if is_old {
                // The entry itself: dropped, or under its new name (folder unchanged).
                if let Some(new) = new {
                    let name = new.file_name().and_then(|n| n.to_str()).unwrap_or_else(|| self.name(s));
                    out.push(s.parent, name, s.is_dir);
                }
            } else if !gone[s.parent as usize] {
                out.push(s.parent, self.name(s), s.is_dir);
            }
        }
        out.trim();
        out
    }

    fn add_dir(&mut self, path: PathBuf, depth: u8) -> u32 {
        self.dirs.push(path);
        self.depths.push(depth);
        (self.dirs.len() - 1) as u32
    }

    fn push(&mut self, parent: u32, name: &str, is_dir: bool) {
        let norm = fuzzy::normalize(name);
        let chars = norm.chars().filter(|&c| !fuzzy::is_word_break(c)).fold(0, |m, c| m | char_bit(c));
        let initials = fuzzy::words_iter(&norm).filter_map(|w| w.chars().next()).fold(0, |m, c| m | char_bit(c));
        let name_at = self.names.len() as u32;
        self.names.push_str(name);
        let norm_at = self.norms.len() as u32;
        self.norms.push_str(&norm);
        self.slots.push(Slot {
            norm: (norm_at, self.norms.len() as u32),
            name: (name_at, self.names.len() as u32),
            parent,
            is_dir,
            chars,
            initials,
        });
    }

    /// Entries in the order given; their folders are shared (a folder takes
    /// its depth from the first entry in it). `name_norm` is recomputed from
    /// `name`. Entries without a parent or a UTF-8 name are dropped.
    #[cfg(test)]
    pub fn from_entries(entries: Vec<super::files::FileEntry>) -> Self {
        let mut ix = Index::default();
        let mut dir_ids: std::collections::HashMap<PathBuf, u32> = Default::default();
        for e in entries {
            let Some(parent) = e.path.parent() else { continue };
            let id = match dir_ids.get(parent) {
                Some(&id) => id,
                None => {
                    let id = ix.add_dir(parent.to_path_buf(), e.depth.saturating_sub(1));
                    dir_ids.insert(parent.to_path_buf(), id);
                    id
                }
            };
            ix.push(id, &e.name, e.is_dir);
        }
        ix.trim();
        ix
    }

    #[cfg(test)]
    pub fn entries(&self) -> Vec<super::files::FileEntry> {
        self.slots
            .iter()
            .map(|s| super::files::FileEntry {
                path: self.path(s),
                name: self.name(s).to_string(),
                name_norm: self.norm(s).to_string(),
                is_dir: s.is_dir,
                depth: self.depth(s),
            })
            .collect()
    }
}

/// Walk each root breadth-first (so shallow entries win when its share is
/// hit), down to `max_depth` levels below it. Each root may add at most
/// `max_entries / roots.len()` entries, so one huge root cannot crowd out the
/// others (a root that uses less does not donate the rest), and one directory
/// listing adds at most `max_per_folder` (the rest of that listing is skipped;
/// sub-folders already read from it are still walked). Skips hidden and system
/// entries, dot-names and [`JUNK_DIRS`]; never descends into symlinks or
/// junctions. Missing or unreadable folders are skipped.
///
/// `profile` is the user folder: when it is one of the roots, the ranking
/// depth of its entries counts from its sub-folders (as if Desktop, Documents
/// and the like were roots themselves), so their results rank as before. The
/// walk limits still count from the root.
///
/// `hint` is the size of the previous index (`Sizes::default()` the first
/// time): the vectors start with that plus a small margin, and give back what
/// is left over beyond 5 % at the end.
pub(super) fn scan_index(
    roots: &[PathBuf],
    profile: Option<&Path>,
    max_entries: usize,
    max_depth: usize,
    max_per_folder: usize,
    hint: Sizes,
) -> Index {
    if roots.is_empty() {
        return Index::default();
    }
    let mut ix = Index::with_capacity(hint);
    let share = max_entries / roots.len();
    for root in roots {
        let root_start = ix.len();
        let mut queue: VecDeque<(u32, usize)> = VecDeque::new();
        let id = ix.add_dir(root.clone(), 0);
        let rank_shift = usize::from(profile.is_some_and(|p| same_folder(p, root)));
        queue.push_back((id, 1));
        'walk: while let Some((dir, depth)) = queue.pop_front() {
            let Ok(read) = std::fs::read_dir(&ix.dirs[dir as usize]) else { continue };
            let mut from_this_folder = 0;
            for entry in read.flatten() {
                if ix.len() - root_start >= share {
                    break 'walk;
                }
                if from_this_folder >= max_per_folder {
                    break;
                }
                let file_name = entry.file_name();
                let Some(name) = file_name.to_str() else { continue };
                if name.starts_with('.') {
                    continue;
                }
                // On Windows both come from the directory listing itself: no extra disk access.
                let (Ok(ft), Ok(meta)) = (entry.file_type(), entry.metadata()) else { continue };
                let attrs = meta.file_attributes();
                if skip_by_attributes(attrs) {
                    continue;
                }
                // `is_dir` is false for symlinks and junctions (name-surrogate
                // reparse points), which are listed but not walked. Other reparse
                // points, such as OneDrive placeholder folders, are walked.
                let walk = ft.is_dir();
                let is_dir = attrs & FILE_ATTRIBUTE_DIRECTORY != 0;
                if is_dir && is_junk_dir(name) {
                    continue;
                }
                ix.push(dir, name, is_dir);
                from_this_folder += 1;
                if walk && depth < max_depth {
                    let path = ix.dirs[dir as usize].join(name);
                    let id = ix.add_dir(path, u8::try_from(depth - rank_shift).unwrap_or(u8::MAX));
                    queue.push_back((id, depth + 1));
                }
            }
        }
    }
    ix.trim();
    ix
}

#[cfg(test)]
mod tests {
    use super::same_folder;
    use std::path::Path;

    #[test]
    fn same_folder_ignores_separator_case_and_trailing_slash() {
        assert!(same_folder(Path::new("C:/Users/x"), Path::new(r"C:\Users\x")));
        assert!(same_folder(Path::new(r"c:\users\X\"), Path::new("C:/Users/x")));
        assert!(!same_folder(Path::new(r"C:\Users\x"), Path::new(r"C:\Users\y")));
    }
}
