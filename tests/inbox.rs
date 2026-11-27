use rtm::{
    inbox::{
        actions,
        data::{self, Row, Section},
        resume,
    },
    pick::actions::Io,
    ultimate::Conversation,
};
use serde_json::json;
use std::{
    cell::RefCell,
    io::{Read, Write},
    net::TcpListener,
};
fn question(kind: &str) -> data::Entry {
    data::entries(Some(&json!({"messages":[{"id":7,"to":"user","reply":true,"state":"delivered","text":"Choose","at":100,"questions":[{"text":"Which?","kind":kind,"options":[{"label":"One"},{"label":"Two"}]}]}]})),None).remove(0)
}
fn row() -> Row {
    data::project(
        "local",
        "test",
        "http://127.0.0.1:9999",
        vec![question("choice")],
        vec![],
        None,
        100,
    )
    .remove(0)
}
#[test]
fn sectioning_and_machine_order_and_errors() {
    let now = 86_400_100;
    let mut entries = vec![question("choice"), question("confirm")];
    entries[1].base.at = 101;
    let mut rows = data::project(
        "remote",
        "z",
        "https://a.ts.net",
        entries,
        vec![
            Conversation {
                id: 2,
                busy: true,
                label: Some("Busy".into()),
                ..Default::default()
            },
            Conversation {
                id: 3,
                ..Default::default()
            },
        ],
        None,
        now,
    );
    rows.extend(data::project(
        "local",
        "a",
        "url",
        vec![question("choice")],
        vec![],
        Some("offline".into()),
        now,
    ));
    data::sort(&mut rows, "local");
    assert_eq!(
        rows.iter()
            .filter(|r| r.section == Section::Waiting && !r.error)
            .count(),
        1
    );
    assert_eq!(
        rows.iter().filter(|r| r.section == Section::Quiet).count(),
        2
    );
    assert_eq!(
        rows.iter()
            .filter(|r| r.section == Section::Working)
            .count(),
        1
    );
    assert_eq!(rows[0].host, "local");
    assert!(rows[0].error && rows[0].title.contains("offline"));
    assert_eq!(rows[2].title, "Busy");
    assert_eq!(rows[3].host, "local");
    assert_eq!(rows[4].host, "remote");
    let mut other = rows[3].clone();
    other.project = "z-project".into();
    rows.push(other);
    data::sort(&mut rows, "local");
    assert_eq!(rows[3].project, "a");
    assert_eq!(rows[4].project, "z-project");
    assert_eq!(rows[5].host, "remote");
}
#[test]
fn answer_eligibility_and_payload() {
    for kind in ["choice", "confirm"] {
        assert!(question(kind).answerable().is_some());
    }
    for kind in ["multi", "text", "scale", ""] {
        assert!(question(kind).answerable().is_none());
    }
    let mut e = question("choice");
    e.questions.push(e.questions[0].clone());
    assert!(e.answerable().is_none());
    e.answered = json!({"0":"One"});
    assert!(e.answerable().is_some());
    e.answered = json!({"0":"One","1":"Two"});
    assert!(e.answerable().is_none());
    let r = row();
    assert_eq!(
        actions::request(&r, "answer", Some("Two")).unwrap(),
        (
            "questions/actions/answer".into(),
            json!({"messageId":7,"answers":[{"question":"Which?","answer":"Two"}]})
        )
    );
    assert!(actions::request(&r, "answer", Some("bad")).is_err());
    let mut r = row();
    r.entry = Some(question("confirm"));
    assert_eq!(
        actions::request(&r, "answer", Some("Yes")).unwrap().1["answers"][0]["answer"],
        "Yes"
    );
}
#[test]
fn writes_exact_http_and_guards() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let home = std::env::temp_dir().join(format!("rtm-inbox-test-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::write(
        home.join("settings.json"),
        json!({"remotes":{"test-remote":{"p":url,"bad":"https://example.com"}}}).to_string(),
    )
    .unwrap();
    let server = std::thread::spawn(move || {
        let mut requests = vec![];
        for _ in 0..3 {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut bytes = vec![];
            loop {
                let mut buf = [0; 1024];
                let n = stream.read(&mut buf).unwrap();
                assert!(n > 0);
                bytes.extend_from_slice(&buf[..n]);
                if let Some(end) = bytes.windows(4).position(|b| b == b"\r\n\r\n") {
                    let headers = String::from_utf8_lossy(&bytes[..end]);
                    let length: usize = headers
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length: ")
                                .and_then(|s| s.parse().ok())
                        })
                        .unwrap();
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 19\r\nConnection: close\r\n\r\n{\"result\":{\"id\":7}}").unwrap();
        }
        requests
    });
    let mut r = row();
    r.url = url.clone();
    actions::write(&home, &r, "answer", Some("One")).unwrap();
    actions::write(&home, &r, "dismiss", None).unwrap();
    r.entry.as_mut().unwrap().base.kind = "action".into();
    r.entry.as_mut().unwrap().base.key = "inbox-items:9".into();
    actions::write(&home, &r, "done", None).unwrap();
    let requests = server.join().unwrap();
    for (raw, path, body) in [
        (
            &requests[0],
            "questions/actions/answer",
            json!({"messageId":7,"answers":[{"question":"Which?","answer":"One"}]}),
        ),
        (
            &requests[1],
            "questions/actions/dismiss",
            json!({"messageId":7}),
        ),
        (&requests[2], "inbox-items/actions/done", json!({"id":9})),
    ] {
        assert!(raw.starts_with(&format!("POST /api/extensions/{path} HTTP/1.1")));
        assert!(raw.to_lowercase().contains(&format!("origin: {url}")));
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(raw.split_once("\r\n\r\n").unwrap().1)
                .unwrap(),
            json!({"args":body})
        );
    }
    for url in ["http://127.0.0.1:1", "https://example.com"] {
        r.url = url.into();
        for op in ["answer", "dismiss", "done"] {
            assert!(actions::write(&home, &r, op, Some("One")).is_err());
        }
    }
    std::fs::remove_dir_all(home).unwrap();
}
#[derive(Default)]
struct Fake {
    calls: RefCell<Vec<Vec<String>>>,
    close_error: Option<String>,
}
impl Io for Fake {
    fn http(
        &self,
        _: &str,
        _: &str,
        _: &[(&str, &str)],
        _: Option<&str>,
    ) -> anyhow::Result<String> {
        panic!("unexpected HTTP")
    }
    fn run(&self, _: &str, args: &[&str], _: Option<&str>) -> anyhow::Result<String> {
        self.calls
            .borrow_mut()
            .push(args.iter().map(|s| s.to_string()).collect());
        if args.get(1) == Some(&"block")
            && let Some(error) = &self.close_error
        {
            anyhow::bail!("{error}");
        }
        Ok(json!({"block_ids":["block:viewer"]}).to_string())
    }
}
#[test]
fn viewers_and_split_replace() {
    let mut r = row();
    assert!(actions::viewer(&r).unwrap()[2].ends_with("/?inbox=message:7"));
    r.entry = None;
    r.conversation = Some(42);
    assert!(actions::viewer(&r).unwrap()[2].ends_with("/?conversation=42"));
    r.entry = Some(question("choice"));
    let e = r.entry.as_mut().unwrap();
    e.base.kind = "link".into();
    e.url = Some("https://example.com".into());
    assert_eq!(
        &actions::viewer(&r).unwrap()[1..],
        ["open", "https://example.com"]
    );
    for (ext, program) in [
        ("png", "kitten"),
        ("jpg", "kitten"),
        ("jpeg", "kitten"),
        ("gif", "kitten"),
        ("webp", "kitten"),
        ("md", "glow"),
        ("diff", "delta"),
        ("patch", "delta"),
        ("txt", "bat"),
    ] {
        let e = r.entry.as_mut().unwrap();
        e.base.kind = "artifact".into();
        e.path = Some(format!("/tmp/file ' ;.{ext}"));
        let argv = actions::viewer(&r).unwrap();
        assert!(argv.iter().any(|s| s.ends_with(program)));
        assert_eq!(argv.last().unwrap(), &format!("/tmp/file ' ;.{ext}"));
        if ["md", "diff", "patch"].contains(&ext) {
            assert_eq!(argv[0], "sh");
            assert!(!argv[2].contains("/tmp/"));
            assert!(argv[2].contains("read _"));
        }
    }
    r.entry.as_mut().unwrap().path = Some("/tmp".into());
    assert!(actions::viewer(&r).is_err());
    r.entry.as_mut().unwrap().path = Some("~/test.md".into());
    assert!(
        !actions::viewer(&r)
            .unwrap()
            .last()
            .unwrap()
            .starts_with('~')
    );
    let io = Fake::default();
    let mut previous = None;
    assert!(actions::split(&io, None, &mut previous, &r).is_err());
    actions::split(&io, Some("block:inbox"), &mut previous, &r).unwrap();
    actions::split(&io, Some("block:inbox"), &mut previous, &r).unwrap();
    let calls = io.calls.borrow();
    assert_eq!(calls.len(), 3);
    assert_eq!(
        &calls[0][..11],
        [
            "--autostart=false",
            "split",
            "-b",
            "block:inbox",
            "--split",
            "right",
            "--ratio",
            "50",
            "--focus=false",
            "--json",
            "--"
        ]
    );
    assert_eq!(
        &calls[1][..4],
        ["--autostart=false", "block", "close", "block:viewer"]
    );
    // A session lookup error is not an already-closed block: never create a second viewer.
    let failing = Fake {
        close_error: Some("session session:000 not found (code=session_not_found)".into()),
        ..Default::default()
    };
    assert!(actions::split(&failing, Some("block:inbox"), &mut previous, &r).is_err());
    assert_eq!(previous.as_deref(), Some("block:viewer"));
    assert_eq!(failing.calls.borrow().len(), 1);
}
#[test]
fn resume_exact_bytes_and_plain_sha256() {
    // printf '%s' 'pid=42:time=1700000000:argv=cnRt,aW5ib3g=:TOKEN' | shasum -a 256
    assert_eq!(resume::report(42,1700000000,"TOKEN").as_bytes(),b"\x1b]7577;pid=42:time=1700000000:argv=cnRt,aW5ib3g=:mac=fe28a547dcd0e4f7ad5d5bf8a6b8041bf71df0466a07de69768078a3f15ef217\x1b\\");
}
