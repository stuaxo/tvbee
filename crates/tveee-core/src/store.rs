//! tveee's local cache of what it reads from TVs (the programme guide and
//! its descriptions so far), in SQLite through Diesel.
//!
//! Everything here is a copy of something on a TV, kept with where it came
//! from (`source`, e.g. the broadcast guide) and when it was read
//! (`fetches`), so it can always be read again: there are no migrations.
//! When [`SCHEMA`] changes, the old database is dropped and rebuilt. What
//! only the user has (options, pairings, favourite changes not on the TV
//! yet) stays in files in the config folder instead.
//!
//! The database is `~/.cache/tveee/tveee.sqlite`, shared by every TV (rows
//! carry the TV's address).

use crate::error::{Error, Result};
use crate::provider::Event;
use diesel::prelude::*;
use diesel::sqlite::SqliteConnection;
use std::collections::HashMap;

/// Bump when the tables change: the database is then rebuilt.
const SCHEMA: i32 = 2;

mod schema {
    diesel::table! {
        /// Programmes, from a guide source.
        events (tv, source, channel, start) {
            tv -> Text,
            source -> Text,
            channel -> Integer,
            start -> BigInt,
            ends -> BigInt,
            title -> Text,
            genre -> Nullable<Integer>,
        }
    }

    diesel::table! {
        /// Programme descriptions (read separately from the schedule).
        descriptions (tv, source, channel, start) {
            tv -> Text,
            source -> Text,
            channel -> Integer,
            start -> BigInt,
            info -> Text,
        }
    }

    diesel::table! {
        /// When each kind of data was last read from a source.
        fetches (tv, source, what) {
            tv -> Text,
            source -> Text,
            what -> Text,
            at -> BigInt,
        }
    }
}

use schema::{descriptions, events, fetches};

const CREATE: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS events (
        tv TEXT NOT NULL, source TEXT NOT NULL, channel INTEGER NOT NULL, start BIGINT NOT NULL,
        ends BIGINT NOT NULL, title TEXT NOT NULL, genre INTEGER,
        PRIMARY KEY (tv, source, channel, start))",
    "CREATE TABLE IF NOT EXISTS descriptions (
        tv TEXT NOT NULL, source TEXT NOT NULL, channel INTEGER NOT NULL, start BIGINT NOT NULL,
        info TEXT NOT NULL,
        PRIMARY KEY (tv, source, channel, start))",
    "CREATE TABLE IF NOT EXISTS fetches (
        tv TEXT NOT NULL, source TEXT NOT NULL, what TEXT NOT NULL, at BIGINT NOT NULL,
        PRIMARY KEY (tv, source, what))",
];

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = events)]
struct EventRow {
    tv: String,
    source: String,
    channel: i32,
    start: i64,
    ends: i64,
    title: String,
    genre: Option<i32>,
}

#[derive(Queryable, Selectable, Insertable)]
#[diesel(table_name = descriptions)]
struct DescriptionRow {
    tv: String,
    source: String,
    channel: i32,
    start: i64,
    info: String,
}

#[derive(Insertable)]
#[diesel(table_name = fetches)]
struct FetchRow<'a> {
    tv: &'a str,
    source: &'a str,
    what: &'a str,
    at: i64,
}

fn db_err(e: impl std::fmt::Display) -> Error {
    Error::Failed(format!("tveee's cache database: {e}"))
}

fn path() -> Option<std::path::PathBuf> {
    Some(crate::plugin::cache_path("x")?.with_file_name("tveee.sqlite"))
}

/// Open the database at `path`, (re)building it if it's from another schema.
fn open_at(path: &str) -> Result<SqliteConnection> {
    let mut c = SqliteConnection::establish(path).map_err(db_err)?;
    // the ui reads while its worker writes
    diesel::sql_query("PRAGMA busy_timeout = 5000").execute(&mut c).map_err(db_err)?;
    let version: i32 = diesel::select(diesel::dsl::sql::<diesel::sql_types::Integer>("(SELECT user_version FROM pragma_user_version())"))
        .get_result(&mut c)
        .map_err(db_err)?;
    if version != SCHEMA {
        // only copies of what's on TVs: start again rather than migrate
        for table in ["events", "descriptions", "fetches"] {
            diesel::sql_query(format!("DROP TABLE IF EXISTS {table}")).execute(&mut c).map_err(db_err)?;
        }
        diesel::sql_query(format!("PRAGMA user_version = {SCHEMA}")).execute(&mut c).map_err(db_err)?;
    }
    for sql in CREATE {
        diesel::sql_query(*sql).execute(&mut c).map_err(db_err)?;
    }
    Ok(c)
}

pub fn open() -> Result<SqliteConnection> {
    let Some(p) = path() else { return Err(db_err("no cache folder")) };
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    open_at(&p.to_string_lossy())
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

/// When `what` ("events", "descriptions") was last read from `source` for TV `tv`.
pub fn fetched_at(c: &mut SqliteConnection, tv: &str, source: &str, what: &str) -> Result<Option<i64>> {
    fetches::table
        .filter(fetches::tv.eq(tv).and(fetches::source.eq(source)).and(fetches::what.eq(what)))
        .select(fetches::at)
        .first(c)
        .optional()
        .map_err(db_err)
}

fn mark_fetched(c: &mut SqliteConnection, tv: &str, source: &str, what: &str) -> QueryResult<usize> {
    diesel::replace_into(fetches::table).values(FetchRow { tv, source, what, at: now() }).execute(c)
}

/// Rows a statement: SQLite allows 32766 parameters, and a row takes 7.
const BATCH: usize = 1000;

/// Replace `source`'s programmes for TV `tv` with `list`, noting when.
pub fn save_events(c: &mut SqliteConnection, tv: &str, source: &str, list: &[Event]) -> Result<()> {
    c.transaction(|c| {
        diesel::delete(events::table.filter(events::tv.eq(tv).and(events::source.eq(source)))).execute(c)?;
        for chunk in list.chunks(BATCH) {
            let rows: Vec<EventRow> = chunk
                .iter()
                .map(|e| EventRow {
                    tv: tv.into(),
                    source: source.into(),
                    channel: e.channel as i32,
                    start: e.start,
                    ends: e.end,
                    title: e.title.clone(),
                    genre: e.genre.map(|g| g as i32),
                })
                .collect();
            // a guide can list the same slot twice: keep the last
            diesel::replace_into(events::table).values(&rows).execute(c)?;
        }
        mark_fetched(c, tv, source, "events")?;
        Ok(())
    })
    .map_err(|e: diesel::result::Error| db_err(e))
}

/// `source`'s programmes for TV `tv`, by channel then start.
pub fn load_events(c: &mut SqliteConnection, tv: &str, source: &str) -> Result<Vec<Event>> {
    let rows: Vec<EventRow> = events::table
        .filter(events::tv.eq(tv).and(events::source.eq(source)))
        .order((events::channel, events::start))
        .select(EventRow::as_select())
        .load(c)
        .map_err(db_err)?;
    Ok(rows
        .into_iter()
        .map(|r| Event { channel: r.channel as u32, title: r.title, start: r.start, end: r.ends, genre: r.genre.map(|g| g as u32), info: None })
        .collect())
}

/// Replace `source`'s descriptions for TV `tv`, noting when.
pub fn save_descriptions(c: &mut SqliteConnection, tv: &str, source: &str, list: &[(u32, i64, String)]) -> Result<()> {
    c.transaction(|c| {
        diesel::delete(descriptions::table.filter(descriptions::tv.eq(tv).and(descriptions::source.eq(source)))).execute(c)?;
        for chunk in list.chunks(BATCH) {
            let rows: Vec<DescriptionRow> = chunk
                .iter()
                .map(|(ch, start, info)| DescriptionRow { tv: tv.into(), source: source.into(), channel: *ch as i32, start: *start, info: info.clone() })
                .collect();
            diesel::replace_into(descriptions::table).values(&rows).execute(c)?;
        }
        mark_fetched(c, tv, source, "descriptions")?;
        Ok(())
    })
    .map_err(|e: diesel::result::Error| db_err(e))
}

/// `source`'s descriptions for TV `tv`, by (channel, start).
pub fn load_descriptions(c: &mut SqliteConnection, tv: &str, source: &str) -> Result<HashMap<(u32, i64), String>> {
    let rows: Vec<DescriptionRow> =
        descriptions::table.filter(descriptions::tv.eq(tv).and(descriptions::source.eq(source))).select(DescriptionRow::as_select()).load(c).map_err(db_err)?;
    Ok(rows.into_iter().map(|r| ((r.channel as u32, r.start), r.info)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(channel: u32, start: i64, title: &str) -> Event {
        Event { channel, title: title.into(), start, end: start + 60, genre: Some(6), info: None }
    }

    #[test]
    fn keeps_events_per_tv_and_source() {
        let mut c = open_at(":memory:").unwrap();
        save_events(&mut c, "tv1", "broadcast", &[ev(2, 100, "B"), ev(1, 100, "A"), ev(1, 50, "Z")]).unwrap();
        save_events(&mut c, "tv1", "online", &[ev(1, 100, "Other")]).unwrap();
        save_events(&mut c, "tv2", "broadcast", &[ev(9, 1, "Elsewhere")]).unwrap();
        let got: Vec<_> = load_events(&mut c, "tv1", "broadcast").unwrap().into_iter().map(|e| (e.channel, e.start, e.title)).collect();
        assert_eq!(got, [(1, 50, "Z".into()), (1, 100, "A".into()), (2, 100, "B".into())]);
        // saving again replaces that source's programmes only
        save_events(&mut c, "tv1", "broadcast", &[ev(3, 1, "New")]).unwrap();
        assert_eq!(load_events(&mut c, "tv1", "broadcast").unwrap().len(), 1);
        assert_eq!(load_events(&mut c, "tv1", "online").unwrap()[0].title, "Other");
        assert!(fetched_at(&mut c, "tv1", "broadcast", "events").unwrap().is_some());
        assert!(fetched_at(&mut c, "tv1", "broadcast", "descriptions").unwrap().is_none());
        // a slot listed twice: the last one stays
        save_events(&mut c, "tv3", "broadcast", &[ev(1, 1, "first"), ev(1, 1, "second")]).unwrap();
        assert_eq!(load_events(&mut c, "tv3", "broadcast").unwrap()[0].title, "second");
    }

    #[test]
    fn keeps_descriptions_and_rebuilds_on_a_new_schema() {
        let dir = std::env::temp_dir().join(format!("tveee-store-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.sqlite").to_string_lossy().into_owned();
        let mut c = open_at(&file).unwrap();
        // many rows: more than one batch
        let list: Vec<_> = (0..2500).map(|i| (1, i, format!("about {i}"))).collect();
        save_descriptions(&mut c, "tv1", "broadcast", &list).unwrap();
        let got = load_descriptions(&mut c, "tv1", "broadcast").unwrap();
        assert_eq!((got.len(), got[&(1, 7)].as_str()), (2500, "about 7"));
        // an older schema: opening drops it
        diesel::sql_query("PRAGMA user_version = 0").execute(&mut c).unwrap();
        drop(c);
        let mut c = open_at(&file).unwrap();
        assert!(load_descriptions(&mut c, "tv1", "broadcast").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
