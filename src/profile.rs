//! Turn one fenced plugin dump and an ELF into ranked functions in the repo.
//!
//! Kernel addresses stay in the total and stay out of the candidate list.
//! A function under 1% of the fenced total is discarded. A dump that never
//! lands in a repository function is not filled in with a name the caller
//! already knew.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use object::{Object, ObjectSegment, ObjectSymbol, SegmentFlags};

use crate::selector::FnSpan;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProfileDump {
    pub total: u64,
    pub blocks: Vec<(u64, u64)>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineHit {
    pub file: String,
    pub line: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HotFunction {
    pub name: String,
    pub file: String,
    pub line_start: usize,
    pub line_end: usize,
    pub exclusive: u64,
    pub share: f64,
    pub source: String,
    pub is_test: bool,
}

pub fn parse_dump(text: &str) -> Result<ProfileDump> {
    let mut total = None;
    let mut blocks = Vec::new();
    let mut error = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split_whitespace();
        match parts.next() {
            Some("total") => {
                total = Some(
                    parts
                        .next()
                        .context("total is missing a count")?
                        .parse::<u64>()
                        .context("total is not an integer")?,
                );
            }
            Some("bb") => {
                let pc = parse_pc(parts.next().context("bb is missing a pc")?)?;
                let count = parts
                    .next()
                    .context("bb is missing a count")?
                    .parse::<u64>()
                    .context("bb count is not an integer")?;
                blocks.push((pc, count));
            }
            Some("error") => {
                error = Some(parts.collect::<Vec<_>>().join(" "));
            }
            Some(other) => {
                anyhow::bail!("unknown profile record {other}");
            }
            None => {}
        }
    }
    Ok(ProfileDump {
        total: total.unwrap_or(0),
        blocks,
        error,
    })
}

fn parse_pc(text: &str) -> Result<u64> {
    let hex = text.strip_prefix("0x").unwrap_or(text);
    u64::from_str_radix(hex, 16).with_context(|| format!("bad pc {text}"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct RankedProfile {
    pub functions: Vec<HotFunction>,
    /// At least one guest PC landed in a function in the worktree, before the
    /// 1% filter. Protected paths and the injected fence do not count.
    pub mapped_function: bool,
}

pub fn rank(
    dump: &ProfileDump,
    map_start: u64,
    map_end: u64,
    load_bias: u64,
    locate: impl Fn(u64) -> Option<LineHit>,
    spans_by_file: &HashMap<String, Vec<FnSpan>>,
    worktree: &Path,
    protected: &[String],
    test_name: &str,
) -> RankedProfile {
    rank_with_floor(
        dump,
        map_start,
        map_end,
        load_bias,
        locate,
        spans_by_file,
        worktree,
        protected,
        test_name,
        0.01,
    )
}

pub fn rank_with_floor(
    dump: &ProfileDump,
    map_start: u64,
    map_end: u64,
    load_bias: u64,
    locate: impl Fn(u64) -> Option<LineHit>,
    spans_by_file: &HashMap<String, Vec<FnSpan>>,
    worktree: &Path,
    protected: &[String],
    test_name: &str,
    min_share: f64,
) -> RankedProfile {
    if dump.error.is_some() || dump.total == 0 {
        return RankedProfile {
            functions: Vec::new(),
            mapped_function: false,
        };
    }
    let mut counts: HashMap<(String, String), u64> = HashMap::new();
    let mut mapped_function = false;
    for &(pc, n) in &dump.blocks {
        if pc < map_start || pc >= map_end || n == 0 {
            continue;
        }
        let addr = pc.wrapping_sub(load_bias);
        let Some(hit) = locate(addr) else {
            continue;
        };
        let Some(relative) = repo_relative(&hit.file, worktree) else {
            continue;
        };
        if rejected_path(&relative, protected) {
            continue;
        }
        let Some(spans) = spans_by_file.get(&relative) else {
            continue;
        };
        let Some(span) = innermost(spans, hit.line as usize) else {
            continue;
        };
        if span.source.contains("noctum_fence") || span.source.contains("_noctum_guard") {
            continue;
        }
        mapped_function = true;
        *counts.entry((relative, span.path.clone())).or_default() += n;
    }
    let mut ranked = Vec::new();
    for ((file, name), exclusive) in counts {
        let share = exclusive as f64 / dump.total as f64;
        if share < min_share {
            continue;
        }
        let span = spans_by_file
            .get(&file)
            .and_then(|spans| spans.iter().find(|span| span.path == name));
        let (line_start, line_end, source) = span
            .map(|span| (span.line_start, span.line_end, span.source.clone()))
            .unwrap_or((0, 0, String::new()));
        ranked.push(HotFunction {
            is_test: name == test_name
                || name.ends_with(&format!("::{test_name}"))
                || test_name.ends_with(&format!("::{name}")),
            name,
            file,
            line_start,
            line_end,
            exclusive,
            share,
            source,
        });
    }
    ranked.sort_by(|a, b| b.exclusive.cmp(&a.exclusive).then(a.name.cmp(&b.name)));
    RankedProfile {
        functions: ranked,
        mapped_function,
    }
}

fn innermost(spans: &[FnSpan], line: usize) -> Option<&FnSpan> {
    spans
        .iter()
        .filter(|span| line >= span.line_start && line <= span.line_end)
        .min_by_key(|span| span.line_end.saturating_sub(span.line_start))
}

pub fn repo_relative(file: &str, worktree: &Path) -> Option<String> {
    let path = Path::new(file);
    let relative = if path.is_absolute() {
        path.strip_prefix(worktree).ok()?.to_path_buf()
    } else {
        let joined = worktree.join(path);
        if joined.is_file() {
            path.to_path_buf()
        } else {
            return None;
        }
    };
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn rejected_path(relative: &str, protected: &[String]) -> bool {
    if relative.starts_with("target/")
        || relative.contains("/target/")
        || relative.split('/').any(|part| part == ".git")
    {
        return true;
    }
    protected
        .iter()
        .any(|pattern| glob_match::glob_match(pattern, relative))
}

pub fn symbolize(bytes: &[u8], addrs: &[u64]) -> Result<Vec<Option<LineHit>>> {
    let file = tempfile::NamedTempFile::new().context("dwarf tempfile")?;
    std::fs::write(file.path(), bytes)?;
    let loader =
        addr2line::Loader::new(file.path()).map_err(|err| anyhow::anyhow!("read dwarf: {err}"))?;
    let mut out = Vec::with_capacity(addrs.len());
    for &addr in addrs {
        let location = loader
            .find_location(addr)
            .map_err(|err| anyhow::anyhow!("symbolize {addr:#x}: {err}"))?;
        out.push(location.and_then(|location| {
            location.file.map(|file| LineHit {
                file: file.to_string(),
                line: location.line.unwrap_or(0),
            })
        }));
    }
    Ok(out)
}

pub fn elf_load_bias(bytes: &[u8], mapped_base: u64) -> Result<u64> {
    let elf = object::File::parse(bytes).context("parse measured ELF")?;
    let executable_base = elf
        .segments()
        .filter(|segment| {
            matches!(segment.flags(), SegmentFlags::Elf { p_flags } if p_flags & object::elf::PF_X != 0)
        })
        .filter(|segment| segment.file_range().1 > 0)
        .find_map(|segment| segment.address().checked_sub(segment.file_range().0))
        .context("measured ELF has no executable load segment")?;
    Ok(mapped_base.wrapping_sub(executable_base))
}

pub fn elf_has_repo_line(bytes: &[u8], worktree: &Path) -> Result<bool> {
    let object = object::File::parse(bytes).context("parse elf")?;
    let mut addrs = Vec::new();
    for symbol in object.symbols() {
        if symbol.kind() == object::SymbolKind::Text && symbol.address() != 0 {
            addrs.push(symbol.address());
        }
    }
    addrs.sort_unstable();
    addrs.dedup();
    if addrs.len() > 4000 {
        addrs.truncate(4000);
    }
    let hits = symbolize(bytes, &addrs)?;
    Ok(hits
        .iter()
        .flatten()
        .any(|hit| repo_relative(&hit.file, worktree).is_some()))
}

pub fn elf_is_static(bytes: &[u8]) -> Result<bool> {
    if bytes.len() < 64 || &bytes[0..4] != b"\x7fELF" {
        anyhow::bail!("not an ELF");
    }
    if bytes[4] != 2 || bytes[5] != 1 {
        anyhow::bail!("only little-endian ELF64 harnesses are supported");
    }
    let phoff = read_u64(bytes, 32)? as usize;
    let phentsize = read_u16(bytes, 54)? as usize;
    let phnum = read_u16(bytes, 56)? as usize;
    if phentsize < 4 {
        anyhow::bail!("bad program header size");
    }
    for index in 0..phnum {
        let off = phoff
            .checked_add(
                index
                    .checked_mul(phentsize)
                    .context("program header overflow")?,
            )
            .context("program header overflow")?;
        let kind = read_u32(bytes, off)?;
        if kind == 3 {
            return Ok(false);
        }
    }
    Ok(true)
}

fn read_u16(bytes: &[u8], off: usize) -> Result<u16> {
    let slice = bytes.get(off..off + 2).context("elf truncated")?;
    Ok(u16::from_le_bytes([slice[0], slice[1]]))
}

fn read_u32(bytes: &[u8], off: usize) -> Result<u32> {
    let slice = bytes.get(off..off + 4).context("elf truncated")?;
    Ok(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn read_u64(bytes: &[u8], off: usize) -> Result<u64> {
    let slice = bytes.get(off..off + 8).context("elf truncated")?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn load_bias_uses_the_executable_segment() {
        let mut elf = vec![0u8; 0x2000];
        elf[..7].copy_from_slice(b"\x7fELF\x02\x01\x01");
        elf[16..18].copy_from_slice(&2u16.to_le_bytes());
        elf[18..20].copy_from_slice(&183u16.to_le_bytes());
        elf[20..24].copy_from_slice(&1u32.to_le_bytes());
        elf[32..40].copy_from_slice(&64u64.to_le_bytes());
        elf[52..54].copy_from_slice(&64u16.to_le_bytes());
        elf[54..56].copy_from_slice(&56u16.to_le_bytes());
        elf[56..58].copy_from_slice(&2u16.to_le_bytes());
        for (index, flags, offset, address) in [
            (0, 4u32, 0u64, 0x200000u64),
            (1, 5u32, 0x1000u64, 0x211000u64),
        ] {
            let header = 64 + index * 56;
            elf[header..header + 4].copy_from_slice(&1u32.to_le_bytes());
            elf[header + 4..header + 8].copy_from_slice(&flags.to_le_bytes());
            elf[header + 8..header + 16].copy_from_slice(&offset.to_le_bytes());
            elf[header + 16..header + 24].copy_from_slice(&address.to_le_bytes());
            elf[header + 32..header + 40].copy_from_slice(&0x1000u64.to_le_bytes());
            elf[header + 40..header + 48].copy_from_slice(&0x1000u64.to_le_bytes());
        }
        assert_eq!(elf_load_bias(&elf, 0x210000).unwrap(), 0);
        assert_eq!(elf_load_bias(&elf, 0x7f00210000).unwrap(), 0x7f00000000);
    }

    #[test]
    fn parses_a_dump_and_drops_kernel_pcs() {
        let dump =
            parse_dump("total 1000\nbb 0x1000 700\nbb 0x2000 200\nbb 0xffffffff80000000 100\n")
                .unwrap();
        assert_eq!(dump.total, 1000);
        let mut spans = HashMap::new();
        spans.insert(
            "src/lib.rs".into(),
            vec![
                FnSpan {
                    path: "encode".into(),
                    line_start: 1,
                    line_end: 5,
                    source: "fn encode() {}".into(),
                },
                FnSpan {
                    path: "encode_batch".into(),
                    line_start: 10,
                    line_end: 20,
                    source: "fn encode_batch() {}".into(),
                },
            ],
        );
        let worktree = PathBuf::from("/repo");
        let ranked = rank(
            &dump,
            0x1000,
            0x3000,
            0,
            |addr| match addr {
                0x1000 => Some(LineHit {
                    file: "/repo/src/lib.rs".into(),
                    line: 2,
                }),
                0x2000 => Some(LineHit {
                    file: "/repo/src/lib.rs".into(),
                    line: 12,
                }),
                _ => None,
            },
            &spans,
            &worktree,
            &[],
            "encode_batch",
        );
        assert!(ranked.mapped_function);
        assert_eq!(ranked.functions.len(), 2);
        assert_eq!(ranked.functions[0].name, "encode");
        assert_eq!(ranked.functions[0].exclusive, 700);
        assert!(!ranked.functions[0].is_test);
        assert!(ranked.functions[1].is_test);
    }

    #[test]
    fn discards_a_function_under_one_percent_and_protected_files() {
        let dump = parse_dump("total 10000\nbb 0x10 50\nbb 0x20 9000\n").unwrap();
        let mut spans = HashMap::new();
        spans.insert(
            "src/lib.rs".into(),
            vec![FnSpan {
                path: "encode".into(),
                line_start: 1,
                line_end: 4,
                source: "fn encode() {}".into(),
            }],
        );
        spans.insert(
            "fixtures/data.rs".into(),
            vec![FnSpan {
                path: "load".into(),
                line_start: 1,
                line_end: 3,
                source: "fn load() {}".into(),
            }],
        );
        let ranked = rank(
            &dump,
            0,
            0x100,
            0,
            |addr| match addr {
                0x10 => Some(LineHit {
                    file: "/repo/src/lib.rs".into(),
                    line: 1,
                }),
                0x20 => Some(LineHit {
                    file: "/repo/fixtures/data.rs".into(),
                    line: 1,
                }),
                _ => None,
            },
            &spans,
            Path::new("/repo"),
            &["fixtures/**".into()],
            "encode_batch",
        );
        assert!(
            ranked.mapped_function,
            "a sub-1% in-repo hit is still a mapping"
        );
        assert!(ranked.functions.is_empty(), "{ranked:?}");
        let swept = rank_with_floor(
            &dump,
            0,
            0x100,
            0,
            |addr| {
                Some(LineHit {
                    file: if addr == 0x10 {
                        "/repo/src/lib.rs"
                    } else {
                        "/repo/fixtures/data.rs"
                    }
                    .into(),
                    line: 1,
                })
            },
            &spans,
            Path::new("/repo"),
            &["fixtures/**".into()],
            "encode_batch",
            0.0,
        );
        assert_eq!(swept.functions.len(), 1);
        assert_eq!(swept.functions[0].name, "encode");
        assert_eq!(swept.functions[0].exclusive, 50);
    }

    #[test]
    fn a_second_fence_is_an_error_record() {
        let dump = parse_dump("total 10\nerror second_fence\n").unwrap();
        assert_eq!(dump.error.as_deref(), Some("second_fence"));
        let ranked = rank(
            &dump,
            0,
            10,
            0,
            |_| None,
            &HashMap::new(),
            Path::new("/"),
            &[],
            "t",
        );
        assert!(!ranked.mapped_function);
        assert!(ranked.functions.is_empty());
    }

    #[test]
    fn static_elf_has_no_interpreter() {
        let bytes = std::fs::read("/bin/true").unwrap_or_default();
        if bytes.starts_with(b"\x7fELF") {
            let dynamic = !elf_is_static(&bytes).unwrap();
            assert!(dynamic);
        }
    }
}
