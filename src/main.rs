use anyhow::{Result, bail};
use rtm::{model::Row, seen, snapshot};
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
        Some("seen") if args.len() == 2 => {
            seen::Store::open(seen::path()?)?.mark(&format!("rex:{}", args[1]))?;
        }
        Some("__refresh") if args.len() >= 2 => {
            snapshot::refresh(Path::new(&args[1]), &args[2..])?;
        }
        Some("--help" | "-h") | None => {
            println!("rtm snapshot [--json] [--wait] [--timing]\nrtm seen <rex session id>");
        }
        _ => bail!("usage: rtm snapshot [--json] [--wait] [--timing] | rtm seen <rex session id>"),
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("rtm: {error:#}");
        std::process::exit(1);
    }
}
