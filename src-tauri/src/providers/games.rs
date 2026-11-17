//! Installed games (Steam, Epic, Rockstar, Minecraft) found by name in normal
//! search. The list is scanned on a background thread (`refresh`) and swapped
//! in; a query only reads it.

// Not registered until the launcher wiring; the compiler would call it all dead.
#![allow(dead_code)]

use crate::fuzzy;
use crate::games::{Game, GameSource, Start};
use crate::launch::Launcher;
use crate::provider::{Action, Modifier, Outcome, Provider, Query, ResultItem};
use std::sync::{Arc, RwLock};

pub struct GamesProvider {
    source: Arc<dyn GameSource>,
    launcher: Arc<dyn Launcher>,
    /// `None` until the first scan has been swapped in.
    games: RwLock<Option<Vec<Game>>>,
}

impl GamesProvider {
    pub fn new(source: Arc<dyn GameSource>, launcher: Arc<dyn Launcher>) -> Arc<Self> {
        Arc::new(Self { source, launcher, games: RwLock::new(None) })
    }

    /// A provider whose list is already there and which never scans (tests).
    #[cfg(test)]
    pub fn with_games(launcher: Arc<dyn Launcher>, games: Vec<Game>) -> Arc<Self> {
        struct Nothing;
        impl GameSource for Nothing {
            fn scan(&self) -> Vec<Game> {
                Vec::new()
            }
        }
        Arc::new(Self { source: Arc::new(Nothing), launcher, games: RwLock::new(Some(games)) })
    }

    /// Scans with no lock held, then swaps the list in. Background thread only.
    pub fn refresh(&self) {
        let games = self.source.scan();
        *self.games.write().unwrap_or_else(|e| e.into_inner()) = Some(games);
    }

    fn find(&self, id: &str) -> Option<Game> {
        let games = self.games.read().unwrap_or_else(|e| e.into_inner());
        games.as_ref()?.iter().find(|g| g.id == id).cloned()
    }
}

fn row(g: &Game, score: u32) -> ResultItem {
    ResultItem {
        id: g.id.clone(),
        provider: "games",
        title: g.name.clone(),
        subtitle: g.store.label().into(),
        score,
        icon: g.icon.clone(),
        pinnable: true,
        actions: if g.folder.is_some() { vec![Action::Reveal] } else { vec![] },
        ..Default::default()
    }
}

impl Provider for GamesProvider {
    fn name(&self) -> &'static str {
        "games"
    }

    fn query(&self, q: &Query) -> Vec<ResultItem> {
        let text = q.text.trim();
        if text.is_empty() {
            return Vec::new();
        }
        let games = self.games.read().unwrap_or_else(|e| e.into_inner());
        let mut out: Vec<ResultItem> = games
            .iter()
            .flatten()
            .filter_map(|g| fuzzy::score(text, &g.name).map(|s| row(g, s)))
            .collect();
        out.sort_by(|a, b| b.score.cmp(&a.score));
        out
    }

    fn activate(&self, id: &str, _modifier: Modifier) -> Result<Outcome, String> {
        // Copied out first: the launcher call must not hold the list.
        let game = self.find(id).ok_or_else(|| "game no longer exists".to_string())?;
        match &game.start {
            Start::Uri(uri) => self.launcher.open(uri)?,
            Start::Exe { exe, params } => self.launcher.open_with(exe, params, false)?,
        }
        Ok(Outcome::Hide)
    }

    fn item(&self, id: &str) -> Option<ResultItem> {
        self.find(id).map(|g| row(&g, 0))
    }

    fn ready(&self) -> bool {
        self.games.read().unwrap_or_else(|e| e.into_inner()).is_some()
    }

    fn act(&self, id: &str, action: Action) -> Result<Outcome, String> {
        match action {
            Action::Reveal => {
                let game = self.find(id).ok_or_else(|| "game no longer exists".to_string())?;
                let folder = game.folder.ok_or_else(|| "no install folder".to_string())?;
                self.launcher.reveal(&folder.to_string_lossy())?;
                Ok(Outcome::Hide)
            }
            _ => Err("can't do that here".to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::games::Store;
    use crate::launch::testing::RecordingLauncher;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn q(text: &str) -> Query {
        Query { text: text.into(), scope: None }
    }

    fn steam(appid: &str, name: &str) -> Game {
        Game {
            id: format!("steam:{appid}"),
            name: name.into(),
            store: Store::Steam,
            start: Start::Uri(format!("steam://rungameid/{appid}")),
            folder: Some(PathBuf::from(format!(r"D:\SteamLibrary\steamapps\common\{name}"))),
            icon: r"file:C:\Program Files (x86)\Steam\steam.exe".into(),
        }
    }

    fn rockstar() -> Game {
        Game {
            id: "rockstar:gta5".into(),
            name: "Grand Theft Auto V".into(),
            store: Store::Rockstar,
            start: Start::Exe {
                exe: r"C:\Program Files\Rockstar Games\Launcher\Launcher.exe".into(),
                params: r#"-launchTitleInFolder "D:\Games\GTAV""#.into(),
            },
            folder: Some(PathBuf::from(r"D:\Games\GTAV")),
            icon: r"file:C:\Program Files\Rockstar Games\Launcher\Launcher.exe".into(),
        }
    }

    fn setup() -> (Arc<RecordingLauncher>, Arc<GamesProvider>) {
        let launcher = Arc::new(RecordingLauncher::default());
        let p = GamesProvider::with_games(launcher.clone(), vec![steam("1145360", "Hades"), rockstar()]);
        (launcher, p)
    }

    #[test]
    fn games_are_found_by_name_and_started_by_their_launcher() {
        let (launcher, p) = setup();
        let rows = p.query(&q("hades"));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].title, "Hades");
        assert_eq!(rows[0].subtitle, "Steam");
        assert_eq!(rows[0].provider, "games");
        assert_eq!(p.activate("steam:1145360", Modifier::None), Ok(Outcome::Hide));
        assert_eq!(launcher.calls(), ["open steam://rungameid/1145360"]);

        let rows = p.query(&q("grand theft"));
        assert_eq!(rows[0].subtitle, "Rockstar");
        assert_eq!(p.activate("rockstar:gta5", Modifier::CtrlShift), Ok(Outcome::Hide));
        assert_eq!(
            launcher.calls()[1],
            r#"open_with C:\Program Files\Rockstar Games\Launcher\Launcher.exe elevated=false: -launchTitleInFolder "D:\Games\GTAV""#
        );
        assert!(p.query(&q("")).is_empty());
        assert!(p.activate("steam:0", Modifier::None).is_err());
    }

    #[test]
    fn games_are_learned_and_pinnable() {
        let (_, p) = setup();
        assert_eq!(p.rank_key("steam:1145360"), Some("steam:1145360".to_string()));
        let rebuilt = p.item("steam:1145360").unwrap();
        assert_eq!(rebuilt.title, "Hades");
        assert!(rebuilt.pinnable);
        assert!(p.query(&q("hades"))[0].pinnable);
        assert!(p.item("steam:0").is_none());
    }

    #[test]
    fn open_file_location_reveals_the_install_folder() {
        let launcher = Arc::new(RecordingLauncher::default());
        let mut minecraft = crate::games::minecraft();
        minecraft.folder = None;
        let p = GamesProvider::with_games(launcher.clone(), vec![steam("1145360", "Hades"), minecraft]);
        assert_eq!(p.query(&q("hades"))[0].actions, [Action::Reveal]);
        let mc = p.query(&q("minecraft"));
        assert_eq!(mc[0].subtitle, "Minecraft Launcher");
        assert!(mc[0].actions.is_empty());
        assert_eq!(p.act("steam:1145360", Action::Reveal), Ok(Outcome::Hide));
        assert_eq!(launcher.calls(), [r"reveal D:\SteamLibrary\steamapps\common\Hades"]);
        assert!(p.act("minecraft", Action::Reveal).is_err());
        assert!(p.act("steam:1145360", Action::Copy).is_err());
    }

    #[test]
    fn query_never_scans() {
        struct Counting(Arc<AtomicUsize>);
        impl GameSource for Counting {
            fn scan(&self) -> Vec<Game> {
                self.0.fetch_add(1, Ordering::SeqCst);
                vec![steam("1145360", "Hades")]
            }
        }
        let scans = Arc::new(AtomicUsize::new(0));
        let p = GamesProvider::new(Arc::new(Counting(scans.clone())), Arc::new(RecordingLauncher::default()));
        assert!(!p.ready());
        assert!(p.query(&q("hades")).is_empty());
        assert_eq!(scans.load(Ordering::SeqCst), 0);
        p.refresh();
        assert!(p.ready());
        assert_eq!(scans.load(Ordering::SeqCst), 1);
        for _ in 0..10 {
            assert_eq!(p.query(&q("hades")).len(), 1);
        }
        assert_eq!(scans.load(Ordering::SeqCst), 1);
    }
}
