//! Drive jod-core from the terminal, with no desktop app in the way.
//!
//! This is the headless proof that the core works — and the shape a VPS daemon
//! would take.
//!
//! ```text
//! cargo run -p jod-core --example delegate -- claude_code "say PONG" [model]
//! cargo run -p jod-core --example delegate -- open_code   "say PONG" [model]
//! cargo run -p jod-core --example delegate -- agy         "say PONG" [model]
//! ```
//!
//! For everyday use reach for the `jod` command instead; this stays as the
//! smallest possible thing that exercises the core.

use std::path::PathBuf;

use jod_core::{AgentEvent, HarnessKind, Jod, PermissionPolicy, SpawnRequest};

#[tokio::main]
async fn main() {
    let mut args = std::env::args().skip(1);
    let harness = match args.next().as_deref() {
        Some("claude_code") => HarnessKind::ClaudeCode,
        Some("open_code") => HarnessKind::OpenCode,
        Some("agy") => HarnessKind::Agy,
        other => {
            eprintln!("usage: delegate <claude_code|open_code|agy> <prompt> [model]");
            eprintln!("got: {other:?}");
            std::process::exit(2);
        }
    };
    let prompt = args
        .next()
        .unwrap_or_else(|| "Reply with exactly: PONG".into());
    let model = args.next();

    let jod = Jod::new();

    for h in jod.harnesses() {
        println!(
            "harness {:<12} {:<10} {}",
            h.id,
            if h.available { "available" } else { "MISSING" },
            h.path.unwrap_or_default()
        );
    }
    println!("tmux available: {}\n", jod.tmux_available());

    let mut rx = jod.subscribe();

    let agent = jod
        .spawn_agent(SpawnRequest {
            name: "probe".into(),
            harness,
            prompt,
            cwd: std::env::var("JOD_EXAMPLE_CWD")
                .map(PathBuf::from)
                .unwrap_or_else(|_| jod_core::service::default_cwd()),
            model,
            permission: match std::env::var("JOD_EXAMPLE_PERMISSION").as_deref() {
                Ok("accept_edits") => PermissionPolicy::AcceptEdits,
                Ok("bypass") => PermissionPolicy::Bypass,
                _ => PermissionPolicy::Ask,
            },
            resume: jod_core::Resume::Fresh,
        })
        .await
        .unwrap_or_else(|e| {
            eprintln!("spawn failed: {e}");
            std::process::exit(1);
        });

    println!("agent   {}", agent.id);
    println!("tmux    {}\n", agent.attach_command);

    // Give up rather than hang forever if a harness stalls.
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(300);

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            eprintln!("\ntimed out waiting for the agent");
            std::process::exit(1);
        }
        match tokio::time::timeout(remaining, rx.recv()).await {
            Ok(Ok(envelope)) => {
                let done = matches!(envelope.event, AgentEvent::Finished { .. });
                print_event(&envelope.event);
                if done {
                    let summary = jod.agent(&agent.id).await.expect("agent must exist");
                    println!("\nstatus  {:?}", summary.status);
                    println!("events  {}", summary.event_count);
                    println!("stream  {}", summary.stream_path);
                    return;
                }
            }
            Ok(Err(e)) => {
                eprintln!("event stream ended: {e}");
                return;
            }
            Err(_) => {
                eprintln!("\ntimed out waiting for the agent");
                std::process::exit(1);
            }
        }
    }
}

fn print_event(event: &AgentEvent) {
    match event {
        AgentEvent::Started { session_id, model } => {
            println!(
                "[started]  {} {}",
                model.as_deref().unwrap_or("-"),
                session_id.as_deref().unwrap_or("-")
            );
        }
        AgentEvent::Thinking { text } => println!("[thinking] {}", first_line(text)),
        AgentEvent::Message { text } => println!("[message]  {text}"),
        AgentEvent::ToolCall { name, .. } => println!("[tool →]   {name}"),
        AgentEvent::ToolResult { name, is_error, .. } => {
            println!(
                "[tool ←]   {name}{}",
                if *is_error { " (error)" } else { "" }
            )
        }
        AgentEvent::Raw { line } => println!("[raw]      {}", first_line(line)),
        AgentEvent::Error { message } => println!("[error]    {message}"),
        AgentEvent::Finished {
            text,
            is_error,
            usage,
            exit_code,
        } => {
            println!(
                "[finished] exit={:?} error={} in={:?} out={:?} cost={:?}",
                exit_code, is_error, usage.input_tokens, usage.output_tokens, usage.cost_usd
            );
            if let Some(text) = text {
                println!("[answer]   {text}");
            }
        }
    }
}

fn first_line(s: &str) -> String {
    let line = s.lines().next().unwrap_or("");
    if line.chars().count() > 110 {
        format!("{}…", line.chars().take(110).collect::<String>())
    } else {
        line.to_string()
    }
}
