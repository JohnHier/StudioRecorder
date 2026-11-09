//! End-to-end tests against testserver/audioshare_mock.py (needs python3).

use super::api::*;
use super::queue::*;
use crate::export::{ExportFormat, ExportJob, export};
use crate::wav::WavWriter;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU32};
use std::time::{Duration, Instant};

struct Server {
    child: Child,
    url: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn start_server() -> Server {
    let port = 18000 + (std::process::id() % 1000) as u16;
    let script = concat!(env!("CARGO_MANIFEST_DIR"), "/testserver/audioshare_mock.py");
    let child = Command::new("python3").arg(script).arg(port.to_string()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let t = Instant::now();
    while t.elapsed() < Duration::from_secs(10) {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Server { child, url }
}

fn test_post(url: &str, path: &str, body: Value) -> Value {
    let agent: ureq::Agent = ureq::Agent::config_builder().http_status_as_error(false).build().into();
    let mut r = agent.post(&format!("{url}{path}")).send_json(&body).unwrap();
    serde_json::from_str(&r.body_mut().read_to_string().unwrap()).unwrap()
}

fn make_mp3(name: &str, secs: u32) -> PathBuf {
    let d = std::env::temp_dir().join("studiorec_tests");
    std::fs::create_dir_all(&d).unwrap();
    let wav = d.join(format!("{name}.wav"));
    let mut w = WavWriter::create(&wav, 48000, 2, 24).unwrap();
    let v: Vec<f32> = (0..48000 * secs as usize * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.3).collect();
    w.write_f32(&v).unwrap();
    w.finalize().unwrap();
    let mp3 = d.join(format!("{name}.mp3"));
    let job = ExportJob {
        src: &wav,
        dst: &mp3,
        format: ExportFormat::Mp3,
        segments: vec![(0, 48000 * secs as u64)],
        fade_ms: 0,
        crossfade_ms: 10,
        tags: vec![],
    };
    export(&job, &AtomicU32::new(0), &AtomicBool::new(false)).unwrap();
    mp3
}

fn wait_for(q: &Queue, id: &str, pred: impl Fn(&Job) -> bool, secs: u64) -> Job {
    let t = Instant::now();
    loop {
        let j = q.data.lock().unwrap().jobs.iter().find(|j| j.id == id).cloned().unwrap();
        if pred(&j) {
            return j;
        }
        if t.elapsed() > Duration::from_secs(secs) {
            panic!("timeout, job: {:?} {}", j.state, j.message);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn future_date(days: i64) -> String {
    (chrono::Local::now().date_naive() + chrono::Duration::days(days)).format("%Y-%m-%d").to_string()
}

#[test]
fn full_flow_against_mock_server() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("python3 missing, skipped");
        return;
    }
    let cfg = std::env::temp_dir().join(format!("studiorec_cfg_{}", std::process::id()));
    unsafe {
        std::env::set_var("APPDATA", &cfg);
    }
    let s = start_server();

    // --- pairing
    let code = test_post(&s.url, "/test/claim-code", json!({"user": "Jan de Vries"}));
    let code = code["code"].as_str().unwrap().to_string();
    let c = Client::new(&s.url, None);
    let claim = c.claim(&code.to_lowercase(), "STUDIO-2").unwrap();
    assert_eq!(claim.user.display_name, "Jan de Vries");
    let again = c.claim(&code, "STUDIO-2").unwrap_err();
    assert_eq!(again.code(), "CLAIM_CODE_USED");
    assert!(!again.is_temporary());
    assert_eq!(c.claim("XXXX-XXXX-XXXX", "x").unwrap_err().code(), "INVALID_CLAIM_CODE");

    let c = Client::new(&s.url, Some(claim.token.clone()));
    assert_eq!(c.me().unwrap().user.display_name, "Jan de Vries");
    let progs = c.programs().unwrap();
    assert_eq!(progs.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), vec!["De Blik uur 1", "De Blik uur 2"]);
    assert_eq!(progs[0].quality.min_bitrate_kbps, Some(128));
    assert!(c.upload_info(4).unwrap().suggested_title.starts_with("De Blik uur 1"));

    // --- upload via the queue
    let mp3 = make_mp3("as_upload", 8);
    let q = Queue::start();
    q.tokens.lock().unwrap().insert("p1".into(), (s.url.clone(), claim.token.clone()));
    let base = Job {
        profile_id: "p1".into(),
        profile_name: "Jan de Vries".into(),
        program_id: 4,
        program_name: "De Blik uur 1".into(),
        date: future_date(7),
        title: "Test 1".into(),
        file: mp3.clone(),
        duration_sec: 8.0,
        sample_rate: 48000,
        quality: progs[0].quality.clone(),
        ..Default::default()
    };
    let j1 = base.clone();
    q.add(j1.clone());
    let done = wait_for(&q, &j1.id, |j| j.state == JobState::Done, 20);
    assert_eq!(done.revision, Some(1));

    // --- same date again: own episode -> confirm -> replaced
    let j2 = Job { id: uuid::Uuid::new_v4().to_string(), title: "Test 1 (nieuw)".into(), ..base.clone() };
    q.add(j2.clone());
    let w = wait_for(&q, &j2.id, |j| j.state == JobState::NeedsConfirm, 20);
    assert_eq!(w.confirm_details["current_title"], "Test 1");
    q.confirm_replace(&j2.id);
    let r = wait_for(&q, &j2.id, |j| j.state == JobState::Done, 20);
    assert!(r.message.contains("revisie 2"), "{}", r.message);

    // --- someone else's episode on that date
    let d3 = future_date(14);
    test_post(&s.url, "/test/publish", json!({"program_id": 4, "date": d3, "user": "Pietje Puk"}));
    let j3 = Job { id: uuid::Uuid::new_v4().to_string(), date: d3, ..base.clone() };
    q.add(j3.clone());
    let f = wait_for(&q, &j3.id, |j| j.state == JobState::Failed, 20);
    assert!(f.message.contains("Pietje Puk"), "{}", f.message);

    // --- temporary failure: retried with the same client_upload_id
    test_post(&s.url, "/test/fail-next", json!({"status": 503, "count": 2}));
    let j4 = Job { id: uuid::Uuid::new_v4().to_string(), date: future_date(21), ..base.clone() };
    q.add(j4.clone());
    let p = wait_for(&q, &j4.id, |j| j.attempt >= 1, 20);
    assert!(p.message.contains("opnieuw proberen"), "{}", p.message);
    // don't wait for the real back-off in the test
    for _ in 0..3 {
        q.with(|d| d.jobs.iter_mut().filter(|j| j.id == j4.id).for_each(|j| j.next_try = 0));
        std::thread::sleep(Duration::from_millis(400));
    }
    wait_for(&q, &j4.id, |j| j.state == JobState::Done, 20);

    // --- precheck: too short for the norm
    let short = make_mp3("as_short", 2);
    let j5 = Job { id: uuid::Uuid::new_v4().to_string(), file: short, duration_sec: 2.0, date: future_date(28), ..base.clone() };
    q.add(j5.clone());
    let f5 = wait_for(&q, &j5.id, |j| j.state == JobState::Failed, 20);
    assert!(f5.message.contains("korter dan het minimum"), "{}", f5.message);

    // --- unlinked: 401 -> token dropped, job waits
    test_post(&s.url, "/test/revoke", json!({"user": "Jan de Vries"}));
    let j6 = Job { id: uuid::Uuid::new_v4().to_string(), date: future_date(35), ..base.clone() };
    q.add(j6.clone());
    let t = Instant::now();
    while q.unlinked.lock().unwrap().is_empty() && t.elapsed() < Duration::from_secs(20) {
        std::thread::sleep(Duration::from_millis(100));
    }
    assert_eq!(q.unlinked.lock().unwrap().as_slice(), ["p1".to_string()]);
    assert!(q.tokens.lock().unwrap().is_empty());
    let j = q.data.lock().unwrap().jobs.iter().find(|j| j.id == j6.id).cloned().unwrap();
    assert!(j.state.is_open());

    // log has green, orange and red lines
    let log = q.data.lock().unwrap().log.clone();
    assert!(log.iter().any(|l| l.level == Level::Ok));
    assert!(log.iter().any(|l| l.level == Level::Warn));
    assert!(log.iter().any(|l| l.level == Level::Error));
    let _ = std::fs::remove_dir_all(&cfg);
}

#[test]
fn link_file_is_read() {
    let d = std::env::temp_dir().join("studiorec_tests");
    std::fs::create_dir_all(&d).unwrap();
    let p = d.join("jan-devries.audioshare");
    std::fs::write(
        &p,
        r#"{"type":"audioshare-maker-link","version":1,"url":"https://platform.br6.nl","user":"Jan de Vries","claim_code":"K7QX-M2PL-9RTA","expires":"2026-10-19T11:50:00+02:00"}"#,
    )
    .unwrap();
    let l = read_link_file(&p).unwrap();
    assert_eq!(l.claim_code, "K7QX-M2PL-9RTA");
    assert_eq!(normalize_url("platform.br6.nl/"), "https://platform.br6.nl");
    std::fs::write(&p, "{}").unwrap();
    assert!(read_link_file(&p).is_err());
}

#[test]
fn programme_list_is_read_tolerantly() {
    // as in the spec
    let v = json!({"programs": [{"id": 4, "slug": "DeBlik1", "name": "De Blik uur 1",
        "quality": {"formats": ["mp3", "flac"], "min_bitrate_kbps": 128, "samplerates": [44100, 48000]}}]});
    let p = parse_programs(&v);
    assert_eq!(p.len(), 1);
    assert_eq!(p[0].quality.samplerates, vec![44100, 48000]);
    // variations: ids as text, nulls, bare array, title instead of name, formats as one string
    let v = json!([{"id": "7", "title": "Ochtendshow", "description": null,
        "quality": {"formats": "MP3,FLAC", "min_bitrate_kbps": "128", "samplerates": null, "lufs_min": "-23"}},
        {"name": "zonder id"}]);
    let p = parse_programs(&v);
    assert_eq!(p.len(), 1);
    assert_eq!((p[0].id, p[0].name.as_str()), (7, "Ochtendshow"));
    assert_eq!(p[0].quality.formats, vec!["mp3", "flac"]);
    assert_eq!(p[0].quality.min_bitrate_kbps, Some(128));
    assert_eq!(p[0].quality.lufs_min, Some(-23.0));
    assert!(p[0].quality.samplerates.is_empty());
    assert_eq!(parse_programs(&json!({"data": [{"id": 1, "name": "x"}]})).len(), 1);
    assert!(parse_programs(&json!({"programs": null})).is_empty());
    // as AudioShare actually answers (2026-10-12): samplerates null, extra min_file_mb
    let v = json!({"programs":[{"id":24,"slug":"TestProgram","name":"Test programma","quality":{"formats":["mp3","flac","ogg"],
        "min_bitrate_kbps":128,"samplerates":null,"min_duration_sec":900,"max_duration_sec":3900,"min_file_mb":20,"max_file_mb":200,
        "lufs_min":-20,"lufs_max":-10,"true_peak_max":-1,"duration_applies_to":"part"}}]});
    let p = parse_programs(&v);
    assert_eq!(p[0].quality.min_file_mb, Some(20));
    assert_eq!(p[0].quality.min_duration_sec, Some(900));
    assert!(p[0].quality.samplerates.is_empty());
}
