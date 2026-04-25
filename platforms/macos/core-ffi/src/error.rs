pub fn err_json(code: &str, message: &str) -> String {
    serde_json::json!({
        "ok": false,
        "error": { "code": code, "message": message }
    })
        .to_string()
}

pub fn ok_json<T: serde::Serialize>(data: T) -> String {
    serde_json::json!({
        "ok": true,
        "data": data
    })
        .to_string()
}
