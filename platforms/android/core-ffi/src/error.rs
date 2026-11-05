use serde::Serialize;

/// JSON error envelope mirroring the protocol shape emitted by [`err_json`].
/// Kept as the typed counterpart of the hand-built `json!` envelope above.
#[allow(dead_code)]
#[derive(Serialize)]
pub struct ErrObj<'a> {
	pub code: &'a str,
	pub message: &'a str,
}

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
