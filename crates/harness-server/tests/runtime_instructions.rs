use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::thread;

use harness_server::runtime_instructions::RuntimeInstructions;
use uuid::Uuid;

#[test]
fn published_instructions_are_applied_and_reported_as_changed() {
    let root =
        std::env::temp_dir().join(format!("centaur-runtime-instructions-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let baseline = root.join("AGENTS_BASE.md");
    let target = root.join("AGENTS.md");
    fs::write(&baseline, "Base instructions\n").unwrap();
    fs::write(&target, "Base instructions\n").unwrap();

    let endpoint = serve_json_once(
        r#"{"data":{"revision":"42","content":"Search Notion for current workstreams.","sha256":"ignored","published_at":"2026-09-15T12:00:00Z"}}"#,
    );
    let mut runtime = RuntimeInstructions::new(endpoint, None, baseline.clone(), target.clone());

    let update = runtime.refresh().unwrap();

    assert!(update.changed);
    assert_eq!(update.revision.as_deref(), Some("42"));
    assert_eq!(
        update.content.as_deref(),
        Some("Search Notion for current workstreams.")
    );
    assert_eq!(
        fs::read_to_string(&target).unwrap(),
        "Base instructions\n\n---\n\n[Organization instructions]\nSearch Notion for current workstreams.\n"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn failed_refresh_preserves_last_known_good_prompt() {
    let root =
        std::env::temp_dir().join(format!("centaur-runtime-instructions-{}", Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    let baseline = root.join("AGENTS_BASE.md");
    let target = root.join("AGENTS.md");
    fs::write(&baseline, "Base\n").unwrap();
    fs::write(&target, "Already applied\n").unwrap();
    let unreachable = "http://127.0.0.1:1/api/v1/sandbox/runtime_instructions".to_owned();
    let mut runtime = RuntimeInstructions::new(unreachable, None, baseline, target.clone());

    assert!(runtime.refresh().is_err());
    assert_eq!(fs::read_to_string(&target).unwrap(), "Already applied\n");
    fs::remove_dir_all(root).unwrap();
}

fn serve_json_once(body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 4096];
        let _ = stream.read(&mut request).unwrap();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    format!("http://{address}/api/v1/sandbox/runtime_instructions")
}
