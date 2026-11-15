use rquickjs::{Context, Promise, Runtime, prelude::Func};
use serde::Deserialize;
use serde_json::Value;
use std::cell::Cell;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const PRELUDE: &str = include_str!("prelude.js");
const MEMORY_LIMIT: usize = 1024 * 1024 * 1024;

pub struct Sandbox {
    _runtime: Runtime,
    context: Context,
    deadline: Rc<Cell<Option<Instant>>>,
}

pub struct Outcome {
    pub ok: bool,
    pub value: Value,
    pub error: Option<String>,
    pub failed_call: Option<String>,
    pub line: Option<u64>,
    pub log: Vec<String>,
    pub timed_out: bool,
}

#[derive(Deserialize, Default)]
struct RawOutcome {
    #[serde(default)]
    ok: bool,
    #[serde(default)]
    value: Value,
    #[serde(default)]
    error: Option<String>,
    #[serde(default, rename = "call")]
    failed_call: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    log: Vec<String>,
}

impl Sandbox {
    pub fn new(host: impl Fn(String, String) -> String + 'static, cancel: Arc<AtomicBool>) -> Result<Sandbox, String> {
        let runtime = Runtime::new().map_err(|e| format!("starting QuickJS: {e}"))?;
        runtime.set_memory_limit(MEMORY_LIMIT);
        runtime.set_max_stack_size(4 * 1024 * 1024);
        let deadline: Rc<Cell<Option<Instant>>> = Rc::new(Cell::new(None));
        let watch = deadline.clone();
        runtime.set_interrupt_handler(Some(Box::new(move || {
            cancel.load(Ordering::Relaxed) || watch.get().is_some_and(|d| Instant::now() > d)
        })));
        let context = Context::full(&runtime).map_err(|e| format!("QuickJS context: {e}"))?;
        context.with(|ctx| -> Result<(), String> {
            ctx.globals()
                .set("__host", Func::from(move |name: String, args: String| host(name, args)))
                .map_err(|e| e.to_string())?;
            ctx.eval::<(), _>(PRELUDE).map_err(|e| format!("prelude: {e}"))
        })?;
        Ok(Sandbox {
            _runtime: runtime,
            context,
            deadline,
        })
    }

    pub fn run(&self, code: &str, limit: Duration) -> Outcome {
        self.deadline.set(Some(Instant::now() + limit));
        let source = format!("__run(async () => {{\n{code}\n}})");
        let raw = self.context.with(|ctx| {
            let promise: Result<Promise, _> = ctx.eval_with_options(source.as_str(), {
                let mut o = rquickjs::context::EvalOptions::default();
                o.filename = Some("script.js".into());
                o.strict = false;
                o
            });
            let result: Result<String, String> = match promise {
                Ok(p) => p.finish::<String>().map_err(|e| describe(&ctx, e)),
                Err(e) => Err(describe(&ctx, e)),
            };
            result
        });
        let timed_out = self.deadline.get().is_some_and(|d| Instant::now() > d);
        self.deadline.set(None);
        match raw {
            Ok(text) => {
                let raw: RawOutcome = serde_json::from_str(&text).unwrap_or_default();
                Outcome {
                    ok: raw.ok,
                    value: raw.value,
                    error: raw.error,
                    failed_call: raw.failed_call,
                    line: raw.line,
                    log: raw.log,
                    timed_out,
                }
            }
            Err(error) => Outcome {
                ok: false,
                value: Value::Null,
                error: Some(if timed_out {
                    format!("script stopped after {} s: {error}", limit.as_secs())
                } else {
                    error
                }),
                failed_call: None,
                line: None,
                log: Vec::new(),
                timed_out,
            },
        }
    }
}

fn describe(ctx: &rquickjs::Ctx, error: rquickjs::Error) -> String {
    match error {
        rquickjs::Error::Exception => {
            let thrown = ctx.catch();
            if let Some(exception) = thrown.as_exception() {
                let message = exception.message().unwrap_or_default();
                let line = exception.stack().and_then(|s| {
                    s.lines().find_map(|l| {
                        l.split("script.js:")
                            .nth(1)
                            .and_then(|r| r.split(':').next())
                            .and_then(|n| n.parse::<i64>().ok())
                    })
                });
                match line {
                    Some(n) => format!("{message} (line {})", (n - 1).max(1)),
                    None => message,
                }
            } else {
                format!("{thrown:?}")
            }
        }
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_raw_outcome_deserialization() {
        let text = json!({
            "ok": true,
            "value": {"result": 42},
            "log": ["log 1", "log 2"]
        }).to_string();

        let raw: RawOutcome = serde_json::from_str(&text).unwrap_or_default();
        assert!(raw.ok);
        assert_eq!(raw.value["result"], 42);
        assert_eq!(raw.log, vec!["log 1", "log 2"]);
        assert!(raw.error.is_none());
        assert!(raw.failed_call.is_none());

        let err_text = json!({
            "ok": false,
            "error": "something failed",
            "call": "grep",
            "line": 10,
            "log": ["step 1"]
        }).to_string();

        let raw_err: RawOutcome = serde_json::from_str(&err_text).unwrap_or_default();
        assert!(!raw_err.ok);
        assert_eq!(raw_err.error.as_deref(), Some("something failed"));
        assert_eq!(raw_err.failed_call.as_deref(), Some("grep"));
        assert_eq!(raw_err.line, Some(10));
        assert_eq!(raw_err.log, vec!["step 1"]);
    }

    #[test]
    fn test_sandbox_run_success_and_error() {
        let cancel = Arc::new(AtomicBool::new(false));
        let sandbox = Sandbox::new(|_, _| "{}".into(), cancel).expect("sandbox init");

        let outcome = sandbox.run("console.log('hi'); return 1 + 2;", Duration::from_secs(5));
        assert!(outcome.ok);
        assert_eq!(outcome.value, json!(3));
        assert!(outcome.log.contains(&"hi".to_string()));

        let err_outcome = sandbox.run("throw new Error('boom');", Duration::from_secs(5));
        assert!(!err_outcome.ok);
        assert!(err_outcome.error.unwrap_or_default().contains("boom"));
    }
}
