//! 极简 MCP（Model Context Protocol）stdio 服务端。
//!
//! 只实现给 AI 递工具要用到的最小集：`initialize` / `tools/list` / `tools/call` /
//! `ping`，JSON-RPC 2.0、按行分帧（一行一个 JSON 对象）。不拉 SDK 依赖——
//! 协议就这几个方法，手写反而好测：`handle_line` 是纯函数（工具执行除外），
//! 喂一行进去、吐一行出来，单测直接对答案。
//!
//! 日志只准去 stderr——stdout 是协议通道，混进一行杂物客户端就断连。

use crate::api::{self, Target};
use serde_json::{json, Value};

/// 工具清单。inputSchema 是标准 JSON Schema，客户端拿它约束模型的参数。
pub fn tool_defs() -> Value {
    json!([
        {
            "name": "upload_file",
            "description": "把本机的一个文件上传到云链盘共享目录。remote_path 是共享盘内的目标路径（含文件名），如 报告/月报.pdf",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "local_path": { "type": "string", "description": "本机文件的绝对路径" },
                    "remote_path": { "type": "string", "description": "共享盘内的目标路径（含文件名）" }
                },
                "required": ["local_path", "remote_path"]
            }
        },
        {
            "name": "download_file",
            "description": "从云链盘共享目录下载一个文件到本机",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "remote_path": { "type": "string", "description": "共享盘内的文件路径" },
                    "local_path": { "type": "string", "description": "存到本机的绝对路径" }
                },
                "required": ["remote_path", "local_path"]
            }
        },
        {
            "name": "list_dir",
            "description": "列出云链盘共享目录里某个目录的内容",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "目录路径，空串或省略 = 根目录" }
                }
            }
        },
        {
            "name": "search",
            "description": "在云链盘共享目录里按文件名搜索",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": { "type": "string", "description": "关键词" },
                    "path": { "type": "string", "description": "限定在哪个目录下搜，省略 = 根目录" }
                },
                "required": ["query"]
            }
        },
        {
            "name": "make_dir",
            "description": "在云链盘共享目录里新建目录",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "remote_path": { "type": "string", "description": "要建的目录路径" }
                },
                "required": ["remote_path"]
            }
        },
        {
            "name": "delete",
            "description": "删除云链盘共享目录里的文件或目录（需要共享端开了「允许删除」）",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "remote_path": { "type": "string", "description": "要删的路径" }
                },
                "required": ["remote_path"]
            }
        },
        {
            "name": "status",
            "description": "看云链盘共享是否在线、连的是哪个地址",
            "inputSchema": { "type": "object", "properties": {} }
        }
    ])
}

async fn call_tool(target: &Target, name: &str, args: &Value) -> anyhow::Result<String> {
    let s = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
    match name {
        "upload_file" => api::upload(target, &s("local_path"), &s("remote_path")).await,
        "download_file" => api::download(target, &s("remote_path"), &s("local_path")).await,
        "list_dir" => api::list_dir(target, &s("path")).await,
        "search" => api::search(target, &s("query"), &s("path")).await,
        "make_dir" => api::make_dir(target, &s("remote_path")).await,
        "delete" => api::delete(target, &s("remote_path")).await,
        "status" => api::status(target).await,
        other => anyhow::bail!("没有叫 {other} 的工具"),
    }
}

/// 处理一行输入。通知（没有 id）不回话，返回 None。
pub async fn handle_line(target: &Target, line: &str) -> Option<String> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => return None, // 不是 JSON 的行直接忽略，别把通道弄脏
    };
    let id = msg.get("id")?.clone(); // 通知无 id：到这就返回 None
    let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
    let params = msg.get("params").cloned().unwrap_or(json!({}));

    let body = match method {
        "initialize" => {
            // 协议版本照抄客户端要的：本服务端只用最基础的能力子集，各版本都在
            let version = params
                .get("protocolVersion")
                .and_then(|v| v.as_str())
                .unwrap_or("2025-06-18");
            json!({ "result": {
                "protocolVersion": version,
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "yunpan-agent", "version": env!("CARGO_PKG_VERSION") }
            }})
        }
        "tools/list" => json!({ "result": { "tools": tool_defs() } }),
        "tools/call" => {
            let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(json!({}));
            match call_tool(target, name, &args).await {
                Ok(text) => json!({ "result": {
                    "content": [{ "type": "text", "text": text }],
                    "isError": false
                }}),
                // 业务失败走 isError 的工具结果而不是 JSON-RPC error——
                // 这样模型能读到中文原因并自行调整，而不是客户端层面报协议错
                Err(e) => json!({ "result": {
                    "content": [{ "type": "text", "text": format!("{e:#}") }],
                    "isError": true
                }}),
            }
        }
        "ping" => json!({ "result": {} }),
        other => json!({ "error": { "code": -32601, "message": format!("不支持的方法：{other}") } }),
    };

    let mut resp = json!({ "jsonrpc": "2.0", "id": id });
    for (k, v) in body.as_object().unwrap() {
        resp[k] = v.clone();
    }
    Some(resp.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> Target {
        Target {
            base_url: "http://127.0.0.1:1".into(),
            user: None,
            pass: None,
        }
    }

    #[tokio::test]
    async fn initialize_回显客户端的协议版本() {
        let line = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#;
        let resp: Value =
            serde_json::from_str(&handle_line(&target(), line).await.unwrap()).unwrap();
        assert_eq!(resp["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(resp["result"]["serverInfo"]["name"], "yunpan-agent");
    }

    #[tokio::test]
    async fn tools_list_里有上传工具() {
        let line = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#;
        let resp: Value =
            serde_json::from_str(&handle_line(&target(), line).await.unwrap()).unwrap();
        let names: Vec<&str> = resp["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"upload_file"), "实际工具：{names:?}");
        assert!(names.contains(&"status"));
    }

    #[tokio::test]
    async fn 通知没有_id_不回话() {
        let line = r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#;
        assert!(handle_line(&target(), line).await.is_none());
    }

    #[tokio::test]
    async fn 未知方法回_32601() {
        let line = r#"{"jsonrpc":"2.0","id":3,"method":"resources/list"}"#;
        let resp: Value =
            serde_json::from_str(&handle_line(&target(), line).await.unwrap()).unwrap();
        assert_eq!(resp["error"]["code"], -32601);
    }

    #[tokio::test]
    async fn 连不上服务时工具结果是_iserror_而不是协议错误() {
        let line = r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_dir","arguments":{}}}"#;
        let resp: Value =
            serde_json::from_str(&handle_line(&target(), line).await.unwrap()).unwrap();
        assert_eq!(resp["result"]["isError"], true);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("连不上"), "实际文案：{text}");
    }
}
