//! 支持的格式矩阵与 LibreOffice 导出过滤器定义。

use std::path::Path;

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    Writer,
    Calc,
    Impress,
}

impl Family {
    pub const fn category(self) -> &'static str {
        match self {
            Self::Writer => "DOCUMENT",
            Self::Calc => "SPREADSHEET",
            Self::Impress => "PRESENTATION",
        }
    }
}

/// 启动探测得到的 LibreOffice 能力集合。
///
/// 部分格式依赖较新的过滤器：Markdown（CommonMark）的导入与导出自 LibreOffice
/// 26.2 起才原生提供，更早的版本（如 Debian bookworm 自带的 7.4）完全没有该
/// 过滤器，转换会直接失败。因此相关格式只在探测通过后才对外暴露。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Capabilities {
    pub markdown: bool,
}

impl Capabilities {
    /// 未探测或探测失败时的最小能力集合。
    pub const NONE: Self = Self { markdown: false };

    /// 该扩展名在当前能力下是否可用。
    pub fn allows(&self, extension: &str) -> bool {
        extension != MARKDOWN_EXTENSION || self.markdown
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SourceFormat {
    pub extension: &'static str,
    pub family: Family,
    pub targets: &'static [&'static str],
}

const MARKDOWN_EXTENSION: &str = "md";

const WRITER_TARGETS: &[&str] = &["pdf", "docx", "doc", "odt", "rtf", "txt", "html", "md"];
const CALC_TARGETS: &[&str] = &["pdf", "xlsx", "xls", "ods", "csv", "html"];
const IMPRESS_TARGETS: &[&str] = &["pdf", "pptx", "ppt", "odp", "png"];

pub const SOURCE_FORMATS: &[SourceFormat] = &[
    SourceFormat {
        extension: "doc",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "docx",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "odt",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "rtf",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "txt",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "html",
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: MARKDOWN_EXTENSION,
        family: Family::Writer,
        targets: WRITER_TARGETS,
    },
    SourceFormat {
        extension: "xls",
        family: Family::Calc,
        targets: CALC_TARGETS,
    },
    SourceFormat {
        extension: "xlsx",
        family: Family::Calc,
        targets: CALC_TARGETS,
    },
    SourceFormat {
        extension: "ods",
        family: Family::Calc,
        targets: CALC_TARGETS,
    },
    SourceFormat {
        extension: "csv",
        family: Family::Calc,
        targets: CALC_TARGETS,
    },
    SourceFormat {
        extension: "ppt",
        family: Family::Impress,
        targets: IMPRESS_TARGETS,
    },
    SourceFormat {
        extension: "pptx",
        family: Family::Impress,
        targets: IMPRESS_TARGETS,
    },
    SourceFormat {
        extension: "odp",
        family: Family::Impress,
        targets: IMPRESS_TARGETS,
    },
];

pub fn source_format(
    extension: &str,
    capabilities: &Capabilities,
) -> Option<&'static SourceFormat> {
    let normalized = normalize_extension(extension)?;
    // LibreOffice 的 Markdown 过滤器同时注册了 `md` 与 `markdown`，对外统一用 `md`。
    let normalized = if normalized == "markdown" {
        MARKDOWN_EXTENSION.to_string()
    } else {
        normalized
    };
    if !capabilities.allows(&normalized) {
        return None;
    }
    SOURCE_FORMATS
        .iter()
        .find(|format| format.extension == normalized)
}

/// 当前能力下可接受的源格式扩展名集合。
pub fn source_extensions(capabilities: &Capabilities) -> Vec<&'static str> {
    SOURCE_FORMATS
        .iter()
        .filter(|format| capabilities.allows(format.extension))
        .map(|format| format.extension)
        .collect()
}

/// 从原始文件名中提取小写扩展名（不含点）。
pub fn file_extension(file_name: &str) -> Option<String> {
    let extension = Path::new(file_name).extension()?.to_str()?;
    normalize_extension(extension)
}

pub fn supports(
    source_extension: &str,
    target_extension: &str,
    capabilities: &Capabilities,
) -> bool {
    let Some(source) = source_format(source_extension, capabilities) else {
        return false;
    };
    let Some(target) = normalize_extension(target_extension) else {
        return false;
    };
    target != source.extension
        && capabilities.allows(&target)
        && source.targets.contains(&target.as_str())
}

/// 某一源格式可用的目标格式（排除与源相同的格式）。
pub fn supported_targets(source: &SourceFormat, capabilities: &Capabilities) -> Vec<&'static str> {
    source
        .targets
        .iter()
        .copied()
        .filter(|target| *target != source.extension && capabilities.allows(target))
        .collect()
}

/// LibreOffice `--convert-to` 所需的过滤器参数。
pub fn output_filter(family: Family, target_extension: &str) -> Option<&'static str> {
    let filter = match (family, normalize_extension(target_extension)?.as_str()) {
        (Family::Writer, "pdf") => "pdf:writer_pdf_Export",
        (Family::Writer, "docx") => "docx:MS Word 2007 XML",
        (Family::Writer, "doc") => "doc:MS Word 97",
        (Family::Writer, "odt") => "odt:writer8",
        (Family::Writer, "rtf") => "rtf:Rich Text Format",
        (Family::Writer, "txt") => "txt:Text (encoded):UTF8",
        (Family::Writer, "html") => "html:HTML (StarWriter)",
        (Family::Writer, MARKDOWN_EXTENSION) => "md:Markdown",
        (Family::Calc, "pdf") => "pdf:calc_pdf_Export",
        (Family::Calc, "xlsx") => "xlsx:Calc MS Excel 2007 XML",
        (Family::Calc, "xls") => "xls:MS Excel 97",
        (Family::Calc, "ods") => "ods:calc8",
        (Family::Calc, "csv") => "csv:Text - txt - csv (StarCalc):44,34,76,1",
        (Family::Calc, "html") => "html:HTML (StarCalc)",
        (Family::Impress, "pdf") => "pdf:impress_pdf_Export",
        (Family::Impress, "pptx") => "pptx:Impress MS PowerPoint 2007 XML",
        (Family::Impress, "ppt") => "ppt:MS PowerPoint 97",
        (Family::Impress, "odp") => "odp:impress8",
        (Family::Impress, "png") => "png:impress_png_Export",
        _ => return None,
    };
    Some(filter)
}

pub fn content_type(extension: &str) -> String {
    match extension {
        "pdf" => "application/pdf".to_string(),
        "txt" => "text/plain; charset=utf-8".to_string(),
        "csv" => "text/csv; charset=utf-8".to_string(),
        "html" => "text/html; charset=utf-8".to_string(),
        "md" => "text/markdown; charset=utf-8".to_string(),
        other => mime_guess::from_ext(other)
            .first_or_octet_stream()
            .as_ref()
            .to_string(),
    }
}

pub fn output_file_name(source_file_name: &str, target_extension: &str) -> String {
    let sanitized = sanitize_file_name(source_file_name);
    let stem = Path::new(&sanitized)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::trim)
        .filter(|stem| !stem.is_empty())
        .unwrap_or("document");
    format!("{}.{}", truncate_chars(stem, 120), target_extension)
}

/// 去除路径与控制字符，仅用于展示和下载文件名。
pub fn sanitize_file_name(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or(raw);
    let cleaned: String = base
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(character, '"' | ':' | '*' | '?' | '<' | '>' | '|')
            {
                '_'
            } else {
                character
            }
        })
        .collect();
    let cleaned = cleaned.trim().trim_matches('.').trim();
    if cleaned.is_empty() {
        return "document".to_string();
    }
    truncate_chars(cleaned, 180)
}

pub fn ascii_file_name(file_name: &str) -> String {
    let fallback: String = file_name
        .chars()
        .map(|character| {
            if character.is_ascii()
                && !character.is_ascii_control()
                && character != '"'
                && character != '\\'
            {
                character
            } else {
                '_'
            }
        })
        .collect();
    if fallback.trim().is_empty() {
        "download".to_string()
    } else {
        fallback
    }
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceFormatView {
    pub format: &'static str,
    pub category: &'static str,
    pub targets: Vec<&'static str>,
}

pub fn catalog(capabilities: &Capabilities) -> Vec<SourceFormatView> {
    SOURCE_FORMATS
        .iter()
        .filter(|format| capabilities.allows(format.extension))
        .map(|format| SourceFormatView {
            format: format.extension,
            category: format.family.category(),
            targets: supported_targets(format, capabilities),
        })
        .collect()
}

fn normalize_extension(extension: &str) -> Option<String> {
    let normalized = extension
        .trim()
        .trim_start_matches('.')
        .to_ascii_lowercase();
    if normalized.is_empty()
        || normalized.len() > 16
        || !normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric())
    {
        return None;
    }
    Some(normalized)
}

fn truncate_chars(value: &str, max_chars: usize) -> String {
    if value.chars().count() <= max_chars {
        return value.to_string();
    }
    value.chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const WITH_MARKDOWN: Capabilities = Capabilities { markdown: true };
    const WITHOUT_MARKDOWN: Capabilities = Capabilities::NONE;

    #[test]
    fn every_advertised_pair_has_a_filter() {
        for source in SOURCE_FORMATS {
            for target in source.targets {
                assert!(
                    output_filter(source.family, target).is_some(),
                    "missing filter for {} -> {}",
                    source.extension,
                    target
                );
            }
        }
    }

    #[test]
    fn resolves_extensions_case_insensitively() {
        assert_eq!(
            file_extension("Quarterly Report.DOCX").as_deref(),
            Some("docx")
        );
        assert!(supports("DOCX", "PDF", &WITH_MARKDOWN));
        assert_eq!(
            source_format(".DocX", &WITH_MARKDOWN).map(|format| format.extension),
            Some("docx")
        );
    }

    #[test]
    fn rejects_same_or_unknown_target() {
        assert!(!supports("docx", "docx", &WITH_MARKDOWN));
        assert!(!supports("docx", "exe", &WITH_MARKDOWN));
        assert!(!supports("exe", "pdf", &WITH_MARKDOWN));
        assert!(supports("xlsx", "csv", &WITH_MARKDOWN));
        assert!(supports("pptx", "png", &WITH_MARKDOWN));
    }

    #[test]
    fn markdown_follows_detected_capabilities() {
        assert!(supports("md", "pdf", &WITH_MARKDOWN));
        assert!(supports("docx", "md", &WITH_MARKDOWN));
        assert!(!supports("md", "pdf", &WITHOUT_MARKDOWN));
        assert!(!supports("docx", "md", &WITHOUT_MARKDOWN));
        assert!(source_format("md", &WITHOUT_MARKDOWN).is_none());
        assert_eq!(
            source_format("markdown", &WITH_MARKDOWN).map(|format| format.extension),
            Some("md")
        );
        assert_eq!(
            file_extension("notes.markdown").as_deref(),
            Some("markdown")
        );

        let enabled = catalog(&WITH_MARKDOWN);
        let markdown = enabled
            .iter()
            .find(|entry| entry.format == "md")
            .expect("md should be advertised");
        assert!(markdown.targets.contains(&"pdf"));
        assert!(!markdown.targets.contains(&"md"));
        assert!(
            enabled
                .iter()
                .any(|entry| entry.format == "docx" && entry.targets.contains(&"md"))
        );

        let disabled = catalog(&WITHOUT_MARKDOWN);
        assert!(disabled.iter().all(|entry| entry.format != "md"));
        assert!(disabled.iter().all(|entry| !entry.targets.contains(&"md")));
        assert_eq!(
            source_extensions(&WITHOUT_MARKDOWN).len(),
            source_extensions(&WITH_MARKDOWN).len() - 1
        );
        assert_eq!(content_type("md"), "text/markdown; charset=utf-8");
    }

    #[test]
    fn catalog_does_not_advertise_identity_conversion() {
        for capabilities in [WITH_MARKDOWN, WITHOUT_MARKDOWN] {
            for entry in catalog(&capabilities) {
                assert!(!entry.targets.contains(&entry.format));
                assert!(!entry.targets.is_empty());
            }
        }
    }

    #[test]
    fn sanitizes_untrusted_file_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("  report.docx  "), "report.docx");
        assert_eq!(sanitize_file_name("..\\..\\evil.docx"), "evil.docx");
        assert_eq!(sanitize_file_name("报告<1>.docx"), "报告_1_.docx");
        assert_eq!(sanitize_file_name("..."), "document");
        assert_eq!(sanitize_file_name(""), "document");
        assert_eq!(output_file_name("报告 v1.docx", "pdf"), "报告 v1.pdf");
        assert_eq!(output_file_name("README.md", "pdf"), "README.pdf");
        assert_eq!(ascii_file_name("报告.pdf"), "__.pdf");
    }
}
