use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use noctum::config::ModelQuant;
use noctum::model::{build_messages, LlamaManager, PromptInput};
use noctum::ports::{ChatMessage, Inferencer};
use noctum::profile::HotFunction;
use serde_json::json;

const SOURCE: &str = include_str!("../qemu/fixtures/observed/src/lib.rs");

fn messages(args: &[String]) -> Result<Vec<ChatMessage>> {
    let source = match args.first() {
        Some(path) => std::fs::read_to_string(path)?,
        None => SOURCE.into(),
    };
    let name = args.get(1).map(String::as_str).unwrap_or("equal_pairs");
    let test_name = args.get(2).map(String::as_str).unwrap_or("encode_batch");
    let span = noctum::selector::function_spans(&source)
        .into_iter()
        .find(|span| span.path == name)
        .context("hot function is missing")?;
    let test = noctum::selector::classify_source(&source)
        .into_iter()
        .find(|test| test.name == test_name)
        .context("test is missing")?;
    let hot = HotFunction {
        name: name.into(),
        file: "src/lib.rs".into(),
        line_start: span.line_start,
        line_end: span.line_end,
        exclusive: 0,
        share: 0.0,
        source: span.source,
        is_test: false,
    };
    Ok(build_messages(&PromptInput {
        test_name,
        test_source: &test.source,
        hot: &hot,
        callee_sources: &[],
        repair_diff: None,
        repair_output: None,
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("request") => {
            let messages: Vec<_> = messages(&args[2..])?
                .iter()
                .map(|m| {
                    json!({
                        "role": m.role, "content": m.content,
                    })
                })
                .collect();
            println!(
                "{}",
                json!({
                    "messages": messages, "temperature": 0.2, "max_tokens": 3072,
                    "response_format": {
                        "type": "json_schema",
                        "json_schema": {
                            "name": "patch", "strict": true,
                            "schema": serde_json::from_str::<serde_json::Value>(
                                include_str!("../grammar/patch_schema.json")
                            )?,
                        },
                    },
                })
            );
        }
        Some("check") => {
            let diff = std::fs::read_to_string(args.get(2).context("diff path is missing")?)?;
            let source = match args.get(3) {
                Some(path) => std::fs::read_to_string(path)?,
                None => SOURCE.into(),
            };
            let test = args.get(4).map(String::as_str).unwrap_or("encode_batch");
            let files = HashMap::from([("src/lib.rs".into(), source)]);
            noctum::diffcheck::check_diff(&diff, &files, Some("src/lib.rs"), test, &[])
                .map_err(anyhow::Error::msg)?;
            if noctum::diffcheck::parse_diff(&diff)
                .map_err(anyhow::Error::msg)?
                .iter()
                .any(|f| f.path != "src/lib.rs")
            {
                bail!("diff changes another file");
            }
        }
        Some("complete") => {
            let data = args.get(2).context("data directory is missing")?;
            let manager = LlamaManager::new(data, ModelQuant::Q2);
            let completion = manager
                .complete(&messages(&args[3..])?, &AtomicBool::new(false))
                .await?;
            println!(
                "{}",
                json!({"summary": completion.summary, "diff": completion.diff})
            );
        }
        Some("bridge") => {
            let data = args.get(2).context("data directory is missing")?;
            let directory = PathBuf::from(args.get(3).context("bridge directory is missing")?);
            let manager = LlamaManager::new(data, ModelQuant::Q2);
            let mut index = 0;
            while !directory.join("stop").exists() {
                let path = directory.join(format!("request-{index}.json"));
                if !path.is_file() {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                    continue;
                }
                let raw: Vec<serde_json::Value> =
                    serde_json::from_str(&std::fs::read_to_string(path)?)?;
                let messages = raw
                    .iter()
                    .map(|m| {
                        Ok(ChatMessage {
                            role: m["role"].as_str().context("role")?.into(),
                            content: m["content"].as_str().context("content")?.into(),
                        })
                    })
                    .collect::<Result<Vec<_>>>()?;
                let begin = std::time::Instant::now();
                let cancel = AtomicBool::new(false);
                let completion = tokio::select! {
                    result = manager.complete(&messages, &cancel) => result,
                    signal = tokio::signal::ctrl_c() => { signal?; break; },
                };
                let response = match completion {
                    Ok(c) => json!({"summary": c.summary, "diff": c.diff}),
                    Err(error) => json!({"error": format!("{error:#}")}),
                };
                let pending = directory.join("pending.json");
                std::fs::write(&pending, serde_json::to_vec_pretty(&response)?)?;
                std::fs::rename(pending, directory.join(format!("response-{index}.json")))?;
                eprintln!(
                    "request {index}: {:.1} seconds",
                    begin.elapsed().as_secs_f64()
                );
                index += 1;
            }
        }
        _ => bail!("use request, check DIFF, complete DATA, or bridge DATA DIRECTORY"),
    }
    Ok(())
}
