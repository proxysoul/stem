use rquickjs::{Context, Promise, Runtime, prelude::Func};
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
                let v: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
                Outcome {
                    ok: v["ok"].as_bool().unwrap_or(false),
                    value: v["value"].clone(),
                    error: v["error"].as_str().map(str::to_string),
                    failed_call: v["call"].as_str().map(str::to_string),
                    line: v["line"].as_u64(),
                    log: v["log"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|l| l.as_str().map(str::to_string))
                        .collect(),
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
