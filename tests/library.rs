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
