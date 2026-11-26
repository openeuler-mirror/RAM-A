//! 定义报告输出格式及其配置和命令行名称。
#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ReportFormat {
    #[serde(rename = "html")]
    Html,
    #[serde(rename = "md")]
    Markdown,
}

impl config_contract::ConfigField for ReportFormat {
    type Overrides = Self;
}

impl ReportFormat {
    pub fn name(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Markdown => "md",
        }
    }
}

impl std::str::FromStr for ReportFormat {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "html" => Ok(Self::Html),
            "md" => Ok(Self::Markdown),
            _ => Err("expected html or md"),
        }
    }
}
