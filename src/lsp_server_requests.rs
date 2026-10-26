//! Every server request receives a response, including string JSON-RPC IDs.
use super::*;

impl LspClient {
    pub(crate) fn configuration(&self, section: Option<&str>) -> Value {
        let Some(section) = section.filter(|section| !section.is_empty()) else {
            return self.settings.clone();
        };
        // Accept both dotted configuration keys and their JSON object form.
        if let Some(value) = self.settings.get(section) {
            return value.clone();
        }
        section
            .split('.')
            .try_fold(&self.settings, |value, key| value.get(key))
            .cloned()
            .unwrap_or(Value::Null)
    }
}

impl LspManager {
    pub(crate) fn handle_server_request(
        &mut self,
        client: usize,
        id: Value,
        method: &str,
        params: Value,
    ) {
        let result = match method {
            "workspace/applyEdit" => {
                match serde_json::from_value::<ApplyWorkspaceEditParams>(params) {
                    Ok(params) => self.workspace_edit_requests.push((self.clients[client].child.id(), id, params.edit)),
                    Err(error) => { let _ = self.clients[client].send_response(id, serde_json::json!({"applied":false,"failureReason":error.to_string()})); }
                }
                return;
            }
            "window/workDoneProgress/create" | "client/registerCapability" | "client/unregisterCapability" => Value::Null,
            "workspace/workspaceFolders" => Url::from_directory_path(&self.workspace_root).ok().map_or(Value::Null, |uri|
                serde_json::json!([{ "uri": uri, "name": self.workspace_root.file_name().unwrap_or_default().to_string_lossy() }])),
            "workspace/configuration" => Value::Array(params.get("items").and_then(Value::as_array)
                .map_or_else(Vec::new, |items| items.iter().map(|item| self.clients[client].configuration(item.get("section").and_then(Value::as_str))).collect())),
            _ => {
                let response = serde_json::json!({ "jsonrpc": "2.0", "id": id,
                    "error": { "code": -32601, "message": format!("Unsupported client method: {method}") } });
                let _ = self.clients[client].send_raw(&response.to_string());
                return;
            }
        };
        let _ = self.clients[client].send_response(id, result);
    }
}
