//! HTML → Word（.docx）：没有 textutil 的系统（Windows）用。只认界面导出 Word 时生成的那几种标签
//! （标题、段落、列表、表格、粗体、斜体、代码），其余标签当成普通文字容器。

use std::path::Path;

use crate::i18n::tr;

#[derive(Clone, Copy, Default, PartialEq)]
enum Style {
    #[default]
    Normal,
    Heading(u8),
}

#[derive(Clone, Default)]
struct Run {
    text: String,
    bold: bool,
    italic: bool,
    code: bool,
    gray: bool,
    /// 这个 run 前面换行（`<br>`、代码块里的换行）
    br: bool,
}

#[derive(Default)]
struct Para {
    style: Style,
    indent: usize,
    runs: Vec<Run>,
}

#[derive(Default)]
struct Cell {
    header: bool,
    paras: Vec<Para>,
}

enum Block {
    Para(Para),
    Table(Vec<Vec<Cell>>),
}

#[derive(Default)]
struct Builder {
    blocks: Vec<Block>,
    para: Para,
    table: Option<Vec<Vec<Cell>>>,
    in_cell: bool,
    lists: Vec<(bool, usize)>,
    bold: usize,
    italic: usize,
    code: usize,
    pre: usize,
    gray: usize,
    skip: usize,
    /// 打开的标签里哪些带了 class="meta"（关掉时要减回去）
    gray_tags: Vec<String>,
}

impl Builder {
    fn flush(&mut self) {
        let mut p = std::mem::take(&mut self.para);
        // 段首段尾的空白不要
        if let Some(r) = p.runs.first_mut() {
            r.text = r.text.trim_start().to_string();
        }
        if let Some(r) = p.runs.last_mut() {
            r.text = r.text.trim_end().to_string();
        }
        p.runs.retain(|r| !r.text.is_empty() || r.br);
        if p.runs.is_empty() {
            return;
        }
        match (&mut self.table, self.in_cell) {
            (Some(rows), true) => {
                if let Some(cell) = rows.last_mut().and_then(|r| r.last_mut()) {
                    cell.paras.push(p);
                }
            }
            _ => self.blocks.push(Block::Para(p)),
        }
    }

    fn text(&mut self, s: &str) {
        if self.skip > 0 {
            return;
        }
        let s = decode_entities(s);
        if self.pre > 0 {
            for (i, line) in s.split('\n').enumerate() {
                self.push_run(line.to_string(), i > 0);
            }
            return;
        }
        // 连续空白并成一个空格；段首、紧跟在空格后面的不要
        let mut space = self.para.runs.last().is_none_or(|r| r.text.ends_with(' '));
        let mut collapsed = String::with_capacity(s.len());
        for c in s.chars() {
            if c.is_whitespace() && c != '\u{3000}' && c != '\u{a0}' {
                if !space {
                    collapsed.push(' ');
                }
                space = true;
            } else {
                space = false;
                collapsed.push(c);
            }
        }
        if collapsed.is_empty() {
            return;
        }
        self.push_run(collapsed, false);
    }

    fn push_run(&mut self, text: String, br: bool) {
        self.para.runs.push(Run {
            text,
            bold: self.bold > 0 || matches!(self.para.style, Style::Heading(_)),
            italic: self.italic > 0,
            code: self.code > 0,
            gray: self.gray > 0,
            br,
        });
    }

    fn open(&mut self, name: &str, attrs: &str) {
        if is_meta(attrs) {
            self.gray += 1;
            self.gray_tags.push(name.to_string());
        }
        match name {
            "head" | "style" | "script" | "title" => self.skip += 1,
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                self.para.style = Style::Heading(name.as_bytes()[1] - b'0');
            }
            "p" | "div" | "blockquote" => self.flush(),
            "pre" => {
                self.flush();
                self.pre += 1;
                self.code += 1;
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.push((name == "ol", 0));
            }
            "li" => {
                self.flush();
                let depth = self.lists.len().max(1);
                let marker = match self.lists.last_mut() {
                    Some((true, n)) => {
                        *n += 1;
                        format!("{n}. ")
                    }
                    _ => "• ".to_string(),
                };
                self.para.indent = depth;
                self.push_run(marker, false);
            }
            "br" => self.push_run(String::new(), true),
            "strong" | "b" => self.bold += 1,
            "em" | "i" => self.italic += 1,
            "code" => self.code += 1,
            "table" => {
                self.flush();
                self.table = Some(Vec::new());
            }
            "tr" => {
                if let Some(rows) = &mut self.table {
                    rows.push(Vec::new());
                }
            }
            "td" | "th" => {
                self.flush();
                if let Some(row) = self.table.as_mut().and_then(|r| r.last_mut()) {
                    row.push(Cell { header: name == "th", paras: Vec::new() });
                    self.in_cell = true;
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        match name {
            "head" | "style" | "script" | "title" => self.skip = self.skip.saturating_sub(1),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "p" | "div" | "blockquote" | "li" => self.flush(),
            "pre" => {
                self.flush();
                self.pre = self.pre.saturating_sub(1);
                self.code = self.code.saturating_sub(1);
            }
            "ul" | "ol" => {
                self.flush();
                self.lists.pop();
            }
            "strong" | "b" => self.bold = self.bold.saturating_sub(1),
            "em" | "i" => self.italic = self.italic.saturating_sub(1),
            "code" => self.code = self.code.saturating_sub(1),
            "td" | "th" => {
                self.flush();
                self.in_cell = false;
            }
            "table" => {
                self.flush();
                self.in_cell = false;
                if let Some(rows) = self.table.take() {
                    let rows: Vec<Vec<Cell>> = rows.into_iter().filter(|r| !r.is_empty()).collect();
                    if !rows.is_empty() {
                        self.blocks.push(Block::Table(rows));
                    }
                }
            }
            _ => {}
        }
        if self.gray_tags.last().is_some_and(|t| t == name) {
            self.gray_tags.pop();
            self.gray = self.gray.saturating_sub(1);
        }
    }
}

fn is_meta(attrs: &str) -> bool {
    let Some(i) = attrs.find("class=") else { return false };
    let rest = attrs[i + 6..].trim_start_matches(['"', '\'']);
    let value = rest.split(['"', '\'']).next().unwrap_or("");
    value.split_whitespace().any(|c| c == "meta")
}

fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[..rest.len().min(12)].find(';');
        let decoded = end.and_then(|e| {
            let name = &rest[1..e];
            let c = match name {
                "amp" => Some('&'),
                "lt" => Some('<'),
                "gt" => Some('>'),
                "quot" => Some('"'),
                "apos" => Some('\''),
                "nbsp" => Some('\u{a0}'),
                _ if name.starts_with("#x") || name.starts_with("#X") => u32::from_str_radix(&name[2..], 16).ok().and_then(char::from_u32),
                _ if name.starts_with('#') => name[1..].parse().ok().and_then(char::from_u32),
                _ => None,
            };
            c.map(|c| (c, e))
        });
        match decoded {
            Some((c, e)) => {
                out.push(c);
                rest = &rest[e + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn parse(html: &str) -> Vec<Block> {
    let mut b = Builder::default();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            b.text(rest);
            break;
        };
        if lt > 0 {
            b.text(&rest[..lt]);
        }
        rest = &rest[lt..];
        if rest.starts_with("<!--") {
            rest = rest.find("-->").map_or("", |e| &rest[e + 3..]);
            continue;
        }
        let Some(gt) = rest.find('>') else {
            b.text(rest);
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];
        if tag.starts_with('!') || tag.starts_with('?') {
            continue;
        }
        let (closing, tag) = match tag.strip_prefix('/') {
            Some(t) => (true, t),
            None => (false, tag),
        };
        let name_end = tag.find(|c: char| c.is_whitespace() || c == '/').unwrap_or(tag.len());
        let name = tag[..name_end].to_ascii_lowercase();
        if closing {
            b.close(&name);
        } else {
            b.open(&name, &tag[name_end..]);
            // 样式、脚本里的内容原样跳过，不当成标签解析
            if name == "style" || name == "script" {
                let end = format!("</{name}");
                let at = rest.to_ascii_lowercase().find(&end).unwrap_or(rest.len());
                rest = &rest[at..];
            }
            if tag.ends_with('/') && name != "br" {
                b.close(&name);
            }
        }
    }
    b.flush();
    if let Some(rows) = b.table.take() {
        b.blocks.push(Block::Table(rows));
    }
    b.blocks
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // XML 1.0 不允许的控制字符
            c if (c as u32) < 0x20 && !matches!(c, '\t' | '\n' | '\r') => {}
            c => out.push(c),
        }
    }
    out
}

fn para_xml(p: &Para, out: &mut String) {
    out.push_str("<w:p><w:pPr>");
    if let Style::Heading(n) = p.style {
        out.push_str(&format!("<w:pStyle w:val=\"Heading{}\"/>", n.clamp(1, 3)));
    }
    if p.indent > 0 {
        out.push_str(&format!("<w:ind w:left=\"{}\" w:hanging=\"300\"/>", 300 + 420 * (p.indent - 1)));
    }
    out.push_str("</w:pPr>");
    for r in &p.runs {
        out.push_str("<w:r><w:rPr>");
        if r.code {
            out.push_str("<w:rFonts w:ascii=\"Consolas\" w:hAnsi=\"Consolas\" w:cs=\"Consolas\"/>");
        }
        if r.bold {
            out.push_str("<w:b/>");
        }
        if r.italic {
            out.push_str("<w:i/>");
        }
        if r.gray {
            out.push_str("<w:color w:val=\"555555\"/>");
        }
        // 字号也直接写在文字上：不读样式表的软件（比如 macOS 的 textutil）照样显示成标题
        if let Style::Heading(n) = p.style {
            let size = [40, 28, 24][n.clamp(1, 3) as usize - 1];
            out.push_str(&format!("<w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/>"));
        }
        out.push_str("</w:rPr>");
        if r.br {
            out.push_str("<w:br/>");
        }
        if !r.text.is_empty() {
            out.push_str(&format!("<w:t xml:space=\"preserve\">{}</w:t>", xml_escape(&r.text)));
        }
        out.push_str("</w:r>");
    }
    out.push_str("</w:p>");
}

fn table_xml(rows: &[Vec<Cell>], out: &mut String) {
    let cols = rows.iter().map(Vec::len).max().unwrap_or(1).max(1);
    let width = 9000 / cols;
    let border = |side: &str| format!("<w:{side} w:val=\"single\" w:sz=\"4\" w:space=\"0\" w:color=\"BBBBBB\"/>");
    out.push_str("<w:tbl><w:tblPr><w:tblW w:w=\"5000\" w:type=\"pct\"/><w:tblBorders>");
    for side in ["top", "left", "bottom", "right", "insideH", "insideV"] {
        out.push_str(&border(side));
    }
    out.push_str("</w:tblBorders><w:tblCellMar><w:top w:w=\"60\" w:type=\"dxa\"/><w:left w:w=\"110\" w:type=\"dxa\"/><w:bottom w:w=\"60\" w:type=\"dxa\"/><w:right w:w=\"110\" w:type=\"dxa\"/></w:tblCellMar></w:tblPr><w:tblGrid>");
    for _ in 0..cols {
        out.push_str(&format!("<w:gridCol w:w=\"{width}\"/>"));
    }
    out.push_str("</w:tblGrid>");
    for row in rows {
        out.push_str("<w:tr>");
        if row.iter().all(|c| c.header) {
            out.push_str("<w:trPr><w:tblHeader/></w:trPr>");
        }
        for i in 0..cols {
            out.push_str(&format!("<w:tc><w:tcPr><w:tcW w:w=\"{width}\" w:type=\"dxa\"/>"));
            let cell = row.get(i);
            if cell.is_some_and(|c| c.header) {
                out.push_str("<w:shd w:val=\"clear\" w:color=\"auto\" w:fill=\"F2F2F5\"/>");
            }
            out.push_str("</w:tcPr>");
            match cell {
                Some(c) if !c.paras.is_empty() => {
                    for p in &c.paras {
                        para_xml(p, out);
                    }
                }
                // 单元格里至少要有一个段落，不然 Word 打不开
                _ => out.push_str("<w:p/>"),
            }
            out.push_str("</w:tc>");
        }
        out.push_str("</w:tr>");
    }
    out.push_str("</w:tbl>");
}

fn document_xml(blocks: &[Block]) -> String {
    let mut body = String::new();
    for b in blocks {
        match b {
            Block::Para(p) => para_xml(p, &mut body),
            Block::Table(rows) => table_xml(rows, &mut body),
        }
    }
    // 最后一块是表格时 Word 要求后面还有一个段落
    if !matches!(blocks.last(), Some(Block::Para(_))) {
        body.push_str("<w:p/>");
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\"><w:body>{body}\
<w:sectPr><w:pgSz w:w=\"11906\" w:h=\"16838\"/><w:pgMar w:top=\"1440\" w:right=\"1300\" w:bottom=\"1440\" w:left=\"1300\" w:header=\"720\" w:footer=\"720\" w:gutter=\"0\"/></w:sectPr>\
</w:body></w:document>"
    )
}

const CONTENT_TYPES: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Types xmlns=\"http://schemas.openxmlformats.org/package/2006/content-types\">\
<Default Extension=\"rels\" ContentType=\"application/vnd.openxmlformats-package.relationships+xml\"/>\
<Default Extension=\"xml\" ContentType=\"application/xml\"/>\
<Override PartName=\"/word/document.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml\"/>\
<Override PartName=\"/word/styles.xml\" ContentType=\"application/vnd.openxmlformats-officedocument.wordprocessingml.styles+xml\"/>\
</Types>";

const ROOT_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument\" Target=\"word/document.xml\"/>\
</Relationships>";

const DOC_RELS: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<Relationships xmlns=\"http://schemas.openxmlformats.org/package/2006/relationships\">\
<Relationship Id=\"rId1\" Type=\"http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles\" Target=\"styles.xml\"/>\
</Relationships>";

fn styles_xml() -> String {
    let heading = |n: u8, size: u32, before: u32| {
        format!(
            "<w:style w:type=\"paragraph\" w:styleId=\"Heading{n}\"><w:name w:val=\"heading {n}\"/><w:basedOn w:val=\"Normal\"/><w:next w:val=\"Normal\"/><w:qFormat/>\
<w:pPr><w:keepNext/><w:spacing w:before=\"{before}\" w:after=\"120\"/><w:outlineLvl w:val=\"{}\"/></w:pPr><w:rPr><w:b/><w:sz w:val=\"{size}\"/><w:szCs w:val=\"{size}\"/></w:rPr></w:style>",
            n - 1
        )
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\n\
<w:styles xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
<w:docDefaults><w:rPrDefault><w:rPr><w:rFonts w:ascii=\"Calibri\" w:hAnsi=\"Calibri\" w:eastAsia=\"Microsoft YaHei\" w:cs=\"Calibri\"/><w:sz w:val=\"22\"/><w:szCs w:val=\"22\"/><w:lang w:val=\"en-US\" w:eastAsia=\"zh-CN\"/></w:rPr></w:rPrDefault>\
<w:pPrDefault><w:pPr><w:spacing w:after=\"80\" w:line=\"360\" w:lineRule=\"auto\"/></w:pPr></w:pPrDefault></w:docDefaults>\
<w:style w:type=\"paragraph\" w:default=\"1\" w:styleId=\"Normal\"><w:name w:val=\"Normal\"/><w:qFormat/></w:style>\
{}{}{}</w:styles>",
        heading(1, 40, 240),
        heading(2, 28, 320),
        heading(3, 24, 200)
    )
}

fn crc32(data: &[u8]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    !data.iter().fold(!0u32, |c, &b| table[((c ^ b as u32) & 0xFF) as usize] ^ (c >> 8))
}

/// 不压缩（stored）的 zip：docx 只有几个小 XML，Word、WPS 都认
fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in files {
        let offset = out.len() as u32;
        let crc = crc32(data);
        let size = data.len() as u32;
        let common = |v: &mut Vec<u8>| {
            v.extend(20u16.to_le_bytes()); // 需要的版本
            v.extend(0x0800u16.to_le_bytes()); // 文件名是 UTF-8
            v.extend(0u16.to_le_bytes()); // stored
            v.extend(0u16.to_le_bytes()); // 时间
            v.extend(0x21u16.to_le_bytes()); // 日期 1980-01-01
            v.extend(crc.to_le_bytes());
            v.extend(size.to_le_bytes());
            v.extend(size.to_le_bytes());
            v.extend((name.len() as u16).to_le_bytes());
            v.extend(0u16.to_le_bytes()); // extra
        };
        out.extend(0x0403_4b50u32.to_le_bytes());
        common(&mut out);
        out.extend(name.as_bytes());
        out.extend(*data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(20u16.to_le_bytes()); // 由哪个版本生成
        common(&mut central);
        central.extend(0u16.to_le_bytes()); // comment
        central.extend(0u16.to_le_bytes()); // disk
        central.extend(0u16.to_le_bytes()); // internal attrs
        central.extend(0u32.to_le_bytes()); // external attrs
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let central_at = out.len() as u32;
    let central_len = central.len() as u32;
    out.extend(central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0u8; 4]);
    out.extend((files.len() as u16).to_le_bytes());
    out.extend((files.len() as u16).to_le_bytes());
    out.extend(central_len.to_le_bytes());
    out.extend(central_at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

/// 把 HTML 排成 .docx 的字节
pub fn render(html: &str) -> Vec<u8> {
    let document = document_xml(&parse(html));
    let styles = styles_xml();
    zip(&[
        ("[Content_Types].xml", CONTENT_TYPES.as_bytes()),
        ("_rels/.rels", ROOT_RELS.as_bytes()),
        ("word/_rels/document.xml.rels", DOC_RELS.as_bytes()),
        ("word/document.xml", document.as_bytes()),
        ("word/styles.xml", styles.as_bytes()),
    ])
}

pub fn html_to_docx(html: &str, out: &Path) -> Result<(), String> {
    std::fs::write(out, render(html))
        .map_err(|e| tr(&format!("导出 Word 失败：{e}"), &format!("Word export failed: {e}")).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"<!doctype html><html><head><meta charset="utf-8"><title>周会 &amp; 复盘</title><style>body{font-family:"PingFang SC"}h1{font-size:20pt}</style></head><body>
  <h1>周会 &amp; 复盘</h1>
  <p class="meta">日期：2026-05-01 14:30　时长：45 分钟</p>
  <h2>结论</h2>
  <ul><li><strong>上线</strong>推迟到 <code>v2.1</code> [12:30]</li><li>预算 &lt; 10 万</li></ul>
  <ol><li>第一步</li><li>第二步</li></ol>
  <table><thead><tr><th>事项</th><th>负责人</th></tr></thead><tbody><tr><td data-label="事项">联调</td><td data-label="负责人">张三</td></tr><tr><td>验收</td><td></td></tr></tbody></table>
  <p>  多个   空格
  和换行</p>
  <p class="meta">由 Tingo 听过 生成</p>
</body></html>"#;

    fn document(html: &str) -> String {
        document_xml(&parse(html))
    }

    #[test]
    fn keeps_structure_and_text() {
        let d = document(SAMPLE);
        assert!(!d.contains("PingFang") && !d.contains("font-size"), "style 内容不应该进正文");
        assert_eq!(d.matches("<w:pStyle w:val=\"Heading1\"/>").count(), 1);
        assert!(d.contains("周会 &amp; 复盘"));
        assert!(d.contains("<w:pStyle w:val=\"Heading2\"/>"));
        assert!(d.contains("<w:b/></w:rPr><w:t xml:space=\"preserve\">上线</w:t>"));
        assert!(d.contains("Consolas") && d.contains(">v2.1<"));
        assert!(d.contains("预算 &lt; 10 万"));
        assert!(d.contains(">• <") && d.contains(">1. <") && d.contains(">2. <"));
        assert_eq!(d.matches("<w:tr>").count(), 3);
        assert!(d.contains("<w:tblHeader/>") && d.contains("w:fill=\"F2F2F5\""));
        assert!(d.contains(">多个 空格 和换行<"));
        assert!(d.contains("<w:color w:val=\"555555\"/>"));
        // 空单元格也得有段落
        assert!(d.contains("</w:tcPr><w:p/></w:tc>"));
    }

    #[test]
    fn zip_is_readable() {
        let bytes = render(SAMPLE);
        assert!(bytes.starts_with(b"PK\x03\x04"));
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
        let dir = std::env::temp_dir().join(format!("tingo-docx-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("纪要.docx");
        html_to_docx(SAMPLE, &out).unwrap();
        // macOS 上用 textutil 反过来读一遍，确认是 Word 认得的文件
        if Path::new("/usr/bin/textutil").is_file() {
            let txt = std::process::Command::new("/usr/bin/textutil").args(["-convert", "txt", "-stdout"]).arg(&out).output().unwrap();
            let txt = String::from_utf8_lossy(&txt.stdout);
            assert!(txt.contains("周会 & 复盘") && txt.contains("张三") && txt.contains("第二步"), "{txt}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }
}
