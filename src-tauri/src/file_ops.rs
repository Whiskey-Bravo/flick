//! What a file or folder row offers in its actions menu. The pure part: the
//! menu entries per kind of row, no disk access.

use crate::provider::Action;

/// The actions menu of a file or folder row, in order. `cloud`: the item is
/// under a cloud sync root (adds the keep/free pair); `drive_root`: `C:\`,
/// which cannot be compressed, renamed, deleted or kept offline.
pub(crate) fn menu(is_dir: bool, cloud: bool, drive_root: bool) -> Vec<Action> {
    let mut actions = if is_dir {
        vec![Action::OpenFolder, Action::OpenTerminal, Action::CopyPath, Action::Compress, Action::Properties]
    } else {
        vec![Action::Reveal, Action::OpenWith, Action::CopyFile, Action::CopyPath, Action::Rename, Action::Delete]
    };
    if cloud {
        actions.extend([Action::KeepOffline, Action::FreeUpSpace]);
    }
    actions.push(Action::MoreOptions);
    if drive_root {
        actions.retain(|a| !matches!(a, Action::Compress | Action::Rename | Action::Delete | Action::KeepOffline | Action::FreeUpSpace));
    }
    actions
}

/// Whether `path` is a drive root: `C:\` or `C:`, any letter case.
pub(crate) fn is_drive_root(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b.len() == 2 || (b.len() == 3 && b[2] == b'\\'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use Action::*;

    #[test]
    fn menu_orders_for_file_and_folder() {
        assert_eq!(menu(true, true, false), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, KeepOffline, FreeUpSpace, MoreOptions]);
        assert_eq!(menu(false, true, false), [Reveal, OpenWith, CopyFile, CopyPath, Rename, Delete, KeepOffline, FreeUpSpace, MoreOptions]);
    }

    #[test]
    fn menu_without_cloud_drops_the_pair() {
        assert_eq!(menu(true, false, false), [OpenFolder, OpenTerminal, CopyPath, Compress, Properties, MoreOptions]);
        assert_eq!(menu(false, false, false), [Reveal, OpenWith, CopyFile, CopyPath, Rename, Delete, MoreOptions]);
    }

    #[test]
    fn menu_on_a_drive_root_drops_changes() {
        assert_eq!(menu(true, false, true), [OpenFolder, OpenTerminal, CopyPath, Properties, MoreOptions]);
        assert_eq!(menu(true, true, true), [OpenFolder, OpenTerminal, CopyPath, Properties, MoreOptions]);
    }

    #[test]
    fn drive_root_rules() {
        assert!(is_drive_root(r"C:\"));
        assert!(is_drive_root("c:"));
        assert!(!is_drive_root(r"C:\x\"));
        assert!(!is_drive_root(r"\\srv\share\"));
        assert!(!is_drive_root(""));
    }
}
