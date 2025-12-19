use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// 收集 Core 发出的 event_json（字符串 JSON），并提供“等待/断言”能力。
pub struct EventCollector {
    buf: Mutex<Vec<Value>>,
    cv: Condvar,
}

impl EventCollector {
    pub fn new() -> Self {
        Self {
            buf: Mutex::new(Vec::new()),
            cv: Condvar::new(),
        }
    }

    pub fn drain(&self) -> Vec<Value> {
        let mut g = self.buf.lock().unwrap();
        std::mem::take(&mut *g)
    }

    pub fn wait_where<F>(&self, timeout: Duration, pred: F) -> Option<Value>
    where
        F: Fn(&Value) -> bool,
    {
        let deadline = Instant::now() + timeout;
        let mut g = self.buf.lock().unwrap();

        loop {
            if let Some(i) = g.iter().position(|v| pred(v)) {
                return Some(g.remove(i));
            }

            let now = Instant::now();
            if now >= deadline {
                return None;
            }

            let wait = deadline - now;
            let (ng, _) = self.cv.wait_timeout(g, wait).unwrap();
            g = ng;
        }
    }

    pub fn wait_type(&self, timeout: Duration, typ: &str) -> Option<Value> {
        self.wait_where(timeout, |v| v.get("type").and_then(|x| x.as_str()) == Some(typ))
    }
}

/// 轻量断言器：基于同一个 EventCollector 做更直观的 assert。
pub struct EventAsserter<'a> {
    c: &'a EventCollector,
}

impl<'a> EventAsserter<'a> {
    pub fn new(c: &'a EventCollector) -> Self {
        Self { c }
    }

    pub fn wait_type(&self, timeout: Duration, typ: &str) -> Value {
        self.c
            .wait_type(timeout, typ)
            .unwrap_or_else(|| panic!("timeout waiting event type={typ}"))
    }

    pub fn wait_where<F>(&self, timeout: Duration, pred: F) -> Value
    where
        F: Fn(&Value) -> bool,
    {
        self.c
            .wait_where(timeout, pred)
            .unwrap_or_else(|| panic!("timeout waiting event predicate"))
    }

    pub fn try_wait_where<F>(&self, timeout: Duration, pred: F) -> Option<Value>
    where
        F: Fn(&Value) -> bool,
    {
        self.c.wait_where(timeout, pred)
    }

    pub fn assert_no_where<F>(&self, duration: Duration, pred: F)
    where
        F: Fn(&Value) -> bool,
    {
        if let Some(v) = self.c.wait_where(duration, pred) {
            panic!("unexpected event: {v}");
        }
    }
}

// 实现 CoreEventSink：把字符串 JSON 解析成 Value 存起来
impl crate::api::CoreEventSink for EventCollector {
    fn emit(&self, event_json: String) {
        if let Ok(v) = serde_json::from_str::<Value>(&event_json) {
            let mut g = self.buf.lock().unwrap();
            g.push(v);
            self.cv.notify_all();
        }
    }
}
