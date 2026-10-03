//! Bounded presentation metadata for the calling Remote connection.

use codex_protocol::mcp::ClientMcpExtensions;
use codex_protocol::mcp::MCP_APP_UI_EXTENSION_ID;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(crate) enum NativeAccountLanguage {
    #[default]
    English,
    Chinese,
}

impl NativeAccountLanguage {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "" | "en" => Some(Self::English),
            "zh" | "zh-CN" | "zh-cn" => Some(Self::Chinese),
            _ => None,
        }
    }

    pub(crate) fn text(self, english: &'static str, chinese: &'static str) -> &'static str {
        match self {
            Self::English => english,
            Self::Chinese => chinese,
        }
    }

    pub(crate) fn other(self) -> Self {
        match self {
            Self::English => Self::Chinese,
            Self::Chinese => Self::English,
        }
    }
}

#[derive(Clone, Copy, Default)]
pub(crate) enum QuestionObservation {
    #[default]
    NotTested,
    Responded,
    MethodNotFound,
    NoAnswer,
    Rejected,
    InvalidAnswer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UiDeclaration {
    Missing,
    Declared,
    Malformed,
}

#[derive(Clone)]
pub(crate) struct NativeAccountCapabilities {
    client_name: String,
    client_version: String,
    ui: UiDeclaration,
    mime_types: Vec<String>,
    pub(crate) question: QuestionObservation,
}

impl NativeAccountCapabilities {
    pub(crate) fn capture(
        client_name: &str,
        client_version: &str,
        _experimental_api: bool,
        extensions: &ClientMcpExtensions,
    ) -> Self {
        let declaration = extensions.get(MCP_APP_UI_EXTENSION_ID);
        let mimes = declaration
            .and_then(serde_json::Value::as_object)
            .and_then(|settings| settings.get("mimeTypes"))
            .and_then(serde_json::Value::as_array);
        let ui = match (declaration, mimes) {
            (None, _) => UiDeclaration::Missing,
            (Some(_), Some(mimes)) if mimes.iter().all(serde_json::Value::is_string) => {
                UiDeclaration::Declared
            }
            (Some(_), _) => UiDeclaration::Malformed,
        };
        let mime_types = mimes
            .into_iter()
            .flatten()
            .filter_map(serde_json::Value::as_str)
            .take(8)
            .map(|mime| bounded_text(mime, /*max_chars*/ 96))
            .collect();
        Self {
            client_name: bounded_text(client_name, /*max_chars*/ 80),
            client_version: bounded_text(client_version, /*max_chars*/ 80),
            ui,
            mime_types,
            question: QuestionObservation::NotTested,
        }
    }

    pub(crate) fn describe(&self, language: NativeAccountLanguage) -> String {
        let question = match self.question {
            QuestionObservation::NotTested => {
                language.text("Not tested on this connection", "此连接尚未测试")
            }
            QuestionObservation::Responded => {
                language.text("Answer received on this connection", "此连接已收到问答回复")
            }
            QuestionObservation::MethodNotFound => language.text(
                "This connection did not handle the question method",
                "此连接未处理问答方法",
            ),
            QuestionObservation::NoAnswer => language.text(
                "No answer received; reopen the menu to retry",
                "未收到回复；可重新打开菜单重试",
            ),
            QuestionObservation::Rejected => language.text(
                "Question request was rejected or disconnected",
                "问答请求被拒绝或连接已断开",
            ),
            QuestionObservation::InvalidAnswer => language.text(
                "Answer format was not accepted; reopen to retry",
                "未接受此回复格式；可重新打开重试",
            ),
        };
        let declaration = match self.ui {
            UiDeclaration::Missing => language.text("Not declared", "未声明"),
            UiDeclaration::Declared => language.text("Declared", "已声明"),
            UiDeclaration::Malformed => language.text("Malformed declaration", "声明格式异常"),
        };
        let mime_types = if self.mime_types.is_empty() {
            language.text("Not declared", "未声明").into()
        } else {
            self.mime_types.join(", ")
        };
        format!("{}\n{}: {} · {}\n{}: {question}\nMCP Apps: {declaration}\nMIME types: {mime_types}\n\n{}\n{}\n{}",
            language.text("In-app controls", "App 内控件"),
            language.text("Client", "客户端"), self.client_name, self.client_version,
            language.text("Native questions", "原生问答"),
            language.text("Open: /account manage [en|zh-CN]", "打开：/account manage [en|zh-CN]"),
            language.text("Native menus show cached account data and do not change accounts or use credits.", "原生菜单显示缓存账号数据，不会修改账号或使用重置券。"),
            language.text("A response confirms the protocol round trip; mobile rendering still needs a device check.", "收到回复可确认协议往返成功；手机界面效果仍需实机确认。"))
    }
}

pub(crate) fn bounded_text(value: &str, max_chars: usize) -> String {
    value.chars().filter(|c| !c.is_control() && !matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        .take(max_chars).collect::<String>().trim().to_string()
}

#[cfg(test)]
#[path = "native_account_capabilities_tests.rs"]
mod tests;
