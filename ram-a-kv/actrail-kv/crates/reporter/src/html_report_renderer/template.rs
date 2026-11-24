//! 本文件声明编译期嵌入的报告根模板及其展示模型入口。

use askama::Template;

use super::view::ReportView;

#[derive(Template)]
#[template(path = "report/layout.html")]
pub(super) struct ReportTemplate<'a> {
    pub(super) report: &'a ReportView,
}
