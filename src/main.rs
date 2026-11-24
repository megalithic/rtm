use anyhow::{Context, Result, bail};
use rtm::{automations, model::Row, needs, pick, seen, snapshot, ultimate};

const USAGE: &str = "rtm pick [<client_id> [<block_id>]] [--bench-first-frame]
rtm kill <session_id> [<client_id>]
rtm snapshot [--json] [--wait] [--timing]
rtm needs [--json]
rtm automation <hub url> <id> <name> pause|resume|run [--json]
rtm seen <rex session id>";
use std::{
    io::{self, Write},
    path::Path,
};

fn print_row(out: &mut impl Write, row: &Row, indent: &str) -> io::Result<()> {
    writeln!(
        out,
        "{indent}{}{} [{}]{}",
        if row.attention { "! " } else { "  " },
        row.name,
        row.state,
        row.metadata
            .branch
            .as_ref()
            .map_or(String::new(), |branch| format!(" {branch}"))
    )?;
    for child in &row.children {
        print_row(out, child, "    ")?;
    }
    Ok(())
}

fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("snapshot") => {
            if args[1..]
                .iter()
                .any(|arg| !matches!(arg.as_str(), "--json" | "--wait" | "--timing"))
            {
                bail!("unknown snapshot option");
            }
            let snapshot = snapshot::load(args.iter().any(|arg| arg == "--wait"))?;
            let mut out = io::stdout().lock();
            if args.iter().any(|arg| arg == "--json") {
                serde_json::to_writer_pretty(&mut out, &snapshot)?;
                writeln!(out)?;
            } else {
                for group in &snapshot.model.groups {
                    writeln!(out, "{:?}", group.kind)?;
                    for row in &group.rows {
                        print_row(&mut out, row, "")?;
                    }
                }
                for warning in &snapshot.warnings {
                    eprintln!("warning: {warning}");
                }
            }
            if args.iter().any(|arg| arg == "--timing") {
                for (source, ms) in &snapshot.timings_ms {
                    eprintln!("{source}: {ms:.2} ms");
                }
            }
        }
        Some("needs") => {
            if args[1..].iter().any(|arg| arg != "--json") {
                bail!("unknown needs option");
            }
            let needs = needs::load(&ultimate::user_home()?)?;
            let mut out = io::stdout().lock();
            if args.len() > 1 {
                serde_json::to_writer(&mut out, &needs)?;
                writeln!(out)?;
            } else {
                writeln!(out, "{} waiting", needs.total)?;
                for hub in &needs.hubs {
                    match (&hub.count, &hub.error) {
                        (Some(count), _) => {
                            writeln!(out, "  {}  {}  {count}", hub.host, hub.project)?;
                            for pinned in hub.automations.iter().filter(|a| a.pinned) {
                                writeln!(
                                    out,
                                    "    {} {}  {}",
                                    pinned.id, pinned.name, pinned.state
                                )?;
                            }
                        }
                        (None, error) => writeln!(
                            out,
                            "  {}  {}  unreachable: {}",
                            hub.host,
                            hub.project,
                            error.as_deref().unwrap_or_default()
                        )?,
                    }
                }
            }
        }
        Some("automation") => {
            let json = args.last().is_some_and(|arg| arg == "--json");
            let [url, id, name, operation] = &args[1..args.len() - usize::from(json)] else {
                bail!("usage: rtm automation <hub url> <id> <name> pause|resume|run [--json]");
            };
            let after = automations::act(url, id, name, operation)?;
            let mut out = io::stdout().lock();
            if json {
                serde_json::to_writer(&mut out, &after)?;
                writeln!(out)?;
            } else {
                writeln!(out, "{} {}  {}", after.id, after.name, after.state)?;
            }
        }
        Some("pick") => {
            let mut positional = Vec::new();
            let mut keys = Vec::new();
            let mut bench = false;
            let mut rest = args[1..].iter();
            while let Some(arg) = rest.next() {
                match arg.as_str() {
                    "--bench-first-frame" => bench = true,
                    // Keys pressed before the first frame, for states a live Rex binding would eat.
                    "--keys" => {
                        for name in rest
                            .next()
                            .map(String::as_str)
                            .unwrap_or_default()
                            .split_whitespace()
                        {
                            keys.push(
                                pick::parse_key(name)
                                    .with_context(|| format!("unknown key {name}"))?,
                            );
                        }
                    }
                    _ if arg.starts_with("--") => bail!("unknown pick option {arg}"),
                    _ => positional.push(arg.clone()),
                }
            }
            pick::run(pick::Options {
                client: positional
                    .first()
                    .cloned()
                    .filter(|value| !value.is_empty()),
                block: positional.get(1).cloned(),
                keys,
                bench,
            })?;
        }
        Some("kill") if args.len() >= 2 => {
            let keys = match args.iter().position(|arg| arg == "--keys") {
                Some(index) => args
                    .get(index + 1)
                    .map(String::as_str)
                    .unwrap_or_default()
                    .split_whitespace()
                    .filter_map(pick::parse_key)
                    .collect(),
                None => Vec::new(),
            };
            pick::kill_prompt(&args[1], keys)?;
        }
        Some("seen") if args.len() == 2 => {
            seen::Store::open(seen::path()?)?.mark(&format!("rex:{}", args[1]))?;
        }
        Some("__refresh") if args.len() >= 2 => {
            snapshot::refresh(Path::new(&args[1]), &args[2..])?;
        }
        Some("--help" | "-h") | None => {
            println!("{USAGE}");
        }
        _ => bail!("usage:\n{USAGE}"),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rtm: {error:#}");
        std::process::exit(1);
    }
}
