//! Managing interviews: filing under companies and roles, rounds, archive, Recently Deleted, and search.

mod common;

use common::fake::*;
use interview_coach::config::{ModelRef, Settings};
use interview_coach::db::Db;
use interview_coach::library;
use interview_coach::models::{Mode, Stage, Status};
use interview_coach::pipeline::analyze_session;
use interview_coach::progress::Quiet;

fn model(name: &str) -> ModelRef {
    format!("anthropic/{name}").parse().unwrap()
}

/// A report that inferred another role title.
fn sample_as(role: &str) -> String {
    sample(false, GOOD_QUOTE).replace("Senior PM", role)
}

/// A report files its interview under the company and role it inferred, once: other models'
/// titles don't re-file it, a role you cleared stays cleared, and a round you set stays set.
#[test]
fn filing_happens_once_and_your_choices_stay() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample_as("Product Lead")), Ok(sample_as("PM"))]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    let s = db.get_session(id).unwrap();
    let role = db.role(s.role_id.expect("filed")).unwrap();
    assert_eq!((role.title.as_str(), role.company.as_deref()), ("Senior PM", Some("Northwind")));
    assert_eq!(s.company, None, "filing never writes the company you'd enter");
    assert_eq!(s.stage, Some(Stage::HiringManager));

    db.set_stage(id, Some(Stage::Technical)).unwrap();
    analyze_session(&mut db, &llm, &model("claude-haiku-4-5"), id, &mut Quiet).unwrap();
    let s = db.get_session(id).unwrap();
    assert_eq!(s.role_id, Some(role.id), "another model's title doesn't re-file it");
    assert_eq!(s.stage, Some(Stage::Technical), "the round you set stays");

    db.set_session_role(id, None).unwrap();
    analyze_session(&mut db, &llm, &model("claude-sonnet-5-5"), id, &mut Quiet).unwrap();
    assert_eq!(db.get_session(id).unwrap().role_id, None, "a role you cleared stays cleared");
    assert_eq!(db.roles().unwrap().len(), 1);
}

/// Upgrading to version 7 files interviews that already have a report.
#[test]
fn upgrading_files_existing_interviews() {
    let (tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE))]);
    analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    drop(db);
    let path = tmp.path().join("coach.db");
    {
        let conn = rusqlite::Connection::open(&path).unwrap();
        conn.execute_batch("DELETE FROM roles; UPDATE sessions SET role_id = NULL, role_set = 0; PRAGMA user_version = 6;").unwrap();
    }
    let db = Db::open(&path).unwrap();
    let s = db.get_session(id).unwrap();
    assert!(s.role_set);
    assert_eq!(db.role(s.role_id.unwrap()).unwrap().title, "Senior PM");
}

/// Nothing is archived or deleted mid-stage; archived and deleted interviews leave the main list
/// but stay listed (flagged) for the app.
#[test]
fn archive_and_delete_wait_for_running_stages_and_leave_the_main_list() {
    let (_tmp, db, id) = setup(Mode::Dual);
    db.set_status(id, Status::Transcribing, None).unwrap();
    assert!(library::archive(&db, id, true).unwrap_err().to_string().contains("transcribing"));
    assert!(library::delete(&db, id).is_err());
    db.set_status(id, Status::Transcribed, None).unwrap();

    library::archive(&db, id, true).unwrap();
    let lib = library::library(&db).unwrap();
    assert!(lib.sessions[0].archived);
    library::archive(&db, id, false).unwrap();

    library::delete(&db, id).unwrap();
    assert_eq!(library::library(&db).unwrap().sessions[0].deleted_days_left, Some(library::DELETED_DAYS));
    library::restore(&db, id).unwrap();
    assert_eq!(library::library(&db).unwrap().sessions[0].deleted_days_left, None);
}

/// Erasing removes a session folder inside the data folder, and never anything outside it.
#[test]
fn erasing_only_removes_folders_inside_the_data_folder() {
    let (tmp, db, outside_id) = setup(Mode::Dual);
    let settings = Settings { data_dir: tmp.path().to_path_buf(), ..Settings::load().unwrap() };
    // The fixture's folder is the data folder itself: outside `sessions/`, so it must survive.
    let inside = db.create_session(interview_coach::db::NewSession {
        title: "Second".into(), company: None, source: interview_coach::models::Source::Upload, mode: Mode::Dual,
        source_path: None, num_speakers: None, consent: None, status: Status::New,
    }).unwrap();
    let folder = settings.sessions_dir().join("0002-second");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("mic.flac"), b"x").unwrap();
    let mut s = db.get_session(inside.id).unwrap();
    s.dir = folder.display().to_string();
    db.save_session(&s).unwrap();

    assert!(library::erase(&db, &settings, inside.id).is_err(), "only from Recently Deleted");
    library::delete(&db, inside.id).unwrap();
    library::delete(&db, outside_id).unwrap();
    // A deleted interview can still be opened and re-run: not erased while that runs.
    db.set_status(inside.id, Status::Transcribing, None).unwrap();
    assert!(library::erase(&db, &settings, inside.id).unwrap_err().to_string().contains("transcribing"));
    assert_eq!(library::empty_deleted(&db, &settings, 0).unwrap(), [outside_id], "the busy one waits");
    assert!(folder.exists());
    db.set_status(inside.id, Status::New, None).unwrap();
    assert_eq!(library::empty_deleted(&db, &settings, library::DELETED_DAYS).unwrap(), Vec::<i64>::new(), "not 30 days yet");
    assert_eq!(library::empty_deleted(&db, &settings, 0).unwrap(), [inside.id]);
    assert!(!folder.exists(), "its folder is gone");
    assert!(tmp.path().exists() && tmp.path().join("coach.db").exists(), "the data folder itself is untouched");
    assert!(db.list_sessions().unwrap().is_empty());
}

#[test]
fn search_finds_titles_and_what_was_said() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let hits = library::search(&db, "ONBOARDING redesign").unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!((hits[0].session_id, hits[0].kind, hits[0].at), (id, "transcript", Some(5.0)));
    assert_eq!(library::search(&db, "hm scr").unwrap()[0].kind, "title");
    assert!(library::search(&db, "100%_").unwrap().is_empty(), "LIKE wildcards are literal");
    library::delete(&db, id).unwrap();
    assert!(library::search(&db, "onboarding").unwrap().is_empty(), "deleted interviews aren't searched");
}

/// The company you enter is an input to the report: changing it makes the next re-run a new
/// version, and the history says why.
#[test]
fn editing_the_company_makes_a_new_version_and_says_so() {
    let (_tmp, mut db, id) = setup(Mode::Dual);
    let llm = FakeLlm::new(vec![Ok(sample(false, GOOD_QUOTE)), Ok(sample(false, GOOD_QUOTE))]);
    let v1 = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    db.set_company(id, Some("Northwind Labs")).unwrap();
    let v2 = analyze_session(&mut db, &llm, &claude(), id, &mut Quiet).unwrap();
    assert_ne!(v1.id, v2.id);
    let history = interview_coach::history::build(&db, id).unwrap();
    assert_eq!(history.version(v2.id).unwrap().changes, ["the company"]);
}

#[test]
fn roles_merge_and_companies_rename_everywhere() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let a = db.find_or_create_role(Some("Northwind"), "Senior PM").unwrap();
    let same = db.find_or_create_role(Some(" northwind "), "senior  pm").unwrap();
    assert_eq!(a.id, same.id, "spelling and spacing don't make a new role");
    let b = db.find_or_create_role(Some("Northwind"), "Product Manager").unwrap();
    db.set_session_role(id, Some(b.id)).unwrap();
    db.merge_role(b.id, a.id).unwrap();
    assert_eq!(db.get_session(id).unwrap().role_id, Some(a.id));
    assert!(db.role(b.id).is_err());
    db.rename_company("NORTHWIND", "Northwind Inc").unwrap();
    assert_eq!(db.role(a.id).unwrap().company.as_deref(), Some("Northwind Inc"));
    assert_eq!(library::archive_company(&db, "northwind inc", true).unwrap(), 1);
    assert!(library::library(&db).unwrap().sessions[0].archived, "archived through its role");
}

fn another(db: &Db, title: &str, company: Option<&str>) -> i64 {
    db.create_session(interview_coach::db::NewSession {
        title: title.into(), company: company.map(String::from), source: interview_coach::models::Source::Upload,
        mode: Mode::Dual, source_path: None, num_speakers: None, consent: None, status: Status::Analyzed,
    })
    .unwrap()
    .id
}

/// Deleting a role takes its rounds to Recently Deleted and hides the role; restoring a round
/// brings the role back, and erasing the last of them removes it for good.
#[test]
fn deleting_a_role_takes_its_rounds_and_restoring_one_brings_it_back() {
    let (tmp, db, id) = setup(Mode::Dual);
    let settings = Settings { data_dir: tmp.path().to_path_buf(), ..Settings::load().unwrap() };
    let role = db.find_or_create_role(Some("Northwind"), "Senior PM").unwrap();
    let second = another(&db, "Onsite", Some("Northwind"));
    db.set_session_role(id, Some(role.id)).unwrap();
    db.set_session_role(second, Some(role.id)).unwrap();

    db.set_status(second, Status::Transcribing, None).unwrap();
    assert!(library::delete_role(&db, role.id).unwrap_err().to_string().contains("transcribing"));
    assert!(db.get_session(id).unwrap().deleted_at.is_none(), "all or nothing");
    db.set_status(second, Status::Analyzed, None).unwrap();

    assert_eq!(library::delete_role(&db, role.id).unwrap(), 2);
    let lib = library::library(&db).unwrap();
    assert!(lib.roles.is_empty(), "a deleted role isn't listed");
    assert!(lib.sessions.iter().all(|s| s.deleted_days_left.is_some()));

    library::restore(&db, second).unwrap();
    let lib = library::library(&db).unwrap();
    assert_eq!(lib.roles.iter().map(|r| r.id).collect::<Vec<_>>(), [role.id], "restoring a round brings its role back");
    assert_eq!(db.get_session(second).unwrap().role_id, Some(role.id));

    library::delete_role(&db, role.id).unwrap();
    library::erase(&db, &settings, id).unwrap();
    assert!(db.role(role.id).is_ok(), "kept while a round is left to restore");
    library::erase(&db, &settings, second).unwrap();
    assert!(db.role(role.id).is_err(), "erased with its last round");
}

/// Filing an interview under a deleted role's title brings that role back.
#[test]
fn a_deleted_role_comes_back_when_something_is_filed_under_it() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let role = db.find_or_create_role(Some("Northwind"), "Senior PM").unwrap();
    db.set_session_role(id, Some(role.id)).unwrap();
    library::delete_role(&db, role.id).unwrap();
    let again = db.find_or_create_role(Some("northwind"), "senior pm").unwrap();
    assert_eq!((again.id, again.deleted_at), (role.id, None));
}

/// Deleting a company takes its roles and its interviews without a role, and nothing else.
#[test]
fn deleting_a_company_takes_its_roles_and_other_interviews() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let role = db.find_or_create_role(Some("Northwind"), "Senior PM").unwrap();
    db.set_session_role(id, Some(role.id)).unwrap();
    let loose = another(&db, "Coffee chat", Some("northwind "));
    let elsewhere = another(&db, "Screen", Some("Acme"));
    assert_eq!(library::delete_company(&db, "NorthWind").unwrap(), 2);
    for (s, deleted) in [(id, true), (loose, true), (elsewhere, false)] {
        assert_eq!(db.get_session(s).unwrap().deleted_at.is_some(), deleted, "session {s}");
    }
    assert!(library::library(&db).unwrap().roles.is_empty());
}

/// The order you arrange things in is kept and listed; renaming a company keeps its place, and
/// resetting goes back to date order.
#[test]
fn arranging_the_sidebar_is_kept_until_reset() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let pm = db.find_or_create_role(Some("Northwind"), "Senior PM").unwrap();
    let lead = db.find_or_create_role(Some("Northwind"), "Product Lead").unwrap();
    let second = another(&db, "Onsite", Some("Northwind"));
    db.set_session_role(id, Some(pm.id)).unwrap();
    db.set_session_role(second, Some(pm.id)).unwrap();

    db.set_role_order(&[lead.id, pm.id]).unwrap();
    db.set_session_order(&[second, id]).unwrap();
    db.set_company_order(&["northwind".into(), "acme".into()]).unwrap();
    assert!(db.set_role_order(&[999]).is_err(), "an unknown role is refused");

    let lib = library::library(&db).unwrap();
    let position = |rid| lib.roles.iter().find(|r| r.id == rid).unwrap().position;
    assert_eq!((position(lead.id), position(pm.id)), (Some(0), Some(1)));
    let position = |sid| lib.sessions.iter().find(|s| s.id == sid).unwrap().position;
    assert_eq!((position(second), position(id)), (Some(0), Some(1)));
    let places = |db: &Db| db.company_order().unwrap();
    assert_eq!(lib.company_order.iter().map(|c| (c.key.as_str(), c.position)).collect::<Vec<_>>(),
               [("northwind", 0), ("acme", 1)]);
    db.set_company_order(&["acme".into()]).unwrap();
    assert_eq!(places(&db), [("acme".into(), 0), ("northwind".into(), 0)], "others keep their places");
    db.set_company_order(&["northwind".into(), "acme".into()]).unwrap();

    db.rename_company("Northwind", "Northwind Labs").unwrap();
    assert_eq!(places(&db), [("northwind labs".into(), 0), ("acme".into(), 1)], "a renamed company keeps its place");
    db.rename_company("Northwind Labs", "Acme").unwrap();
    assert_eq!(places(&db), [("acme".into(), 1)], "merging into a placed company keeps that one's place");

    db.reset_order().unwrap();
    let lib = library::library(&db).unwrap();
    assert!(lib.roles.iter().all(|r| r.position.is_none()) && lib.sessions.iter().all(|s| s.position.is_none()));
    assert!(lib.company_order.is_empty());
}

/// An interview under a role with no company shows under its own company, so deleting that
/// company takes it too.
#[test]
fn deleting_a_company_takes_what_the_sidebar_shows_under_it() {
    let (_tmp, db, id) = setup(Mode::Dual);
    let role = db.find_or_create_role(None, "Engineer").unwrap();
    db.set_company(id, Some("Acme")).unwrap();
    db.set_session_role(id, Some(role.id)).unwrap();
    assert_eq!(library::delete_company(&db, "acme").unwrap(), 1);
    assert!(db.get_session(id).unwrap().deleted_at.is_some());
    assert!(db.role(role.id).unwrap().deleted_at.is_none(), "a role without a company isn't the company's");
}
