//! Just enough `tar` to move small files through `docker cp`, which reads and
//! writes tar streams on a stopped container: write a stream of regular files
//! (with their parent directories), and read the regular files out of one.

use crate::error::VivariumError;

const BLOCK: usize = 512;

/// One regular file of a tar stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TarFile {
    pub path: String,
    pub content: Vec<u8>,
}

/// A tar stream holding `files` (`(absolute guest path, contents)`), rooted at
/// `/`: each path loses its leading slash, and every parent directory gets an
/// entry so the extraction creates it.
///
/// # Errors
/// Returns an error if a path is too long for a plain tar header.
pub fn build(files: &[(String, Vec<u8>)]) -> Result<Vec<u8>, VivariumError> {
    let mut out = Vec::new();
    let mut made: Vec<String> = Vec::new();
    for (path, content) in files {
        let rel = path.trim_start_matches('/');
        let mut dir = String::new();
        let parts: Vec<&str> = rel.split('/').collect();
        for part in &parts[..parts.len().saturating_sub(1)] {
            dir.push_str(part);
            dir.push('/');
            if !made.contains(&dir) {
                out.extend(header(&dir, 0, b'5', 0o755)?);
                made.push(dir.clone());
            }
        }
        out.extend(header(rel, content.len(), b'0', 0o644)?);
        out.extend_from_slice(content);
        out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
    }
    out.resize(out.len() + 2 * BLOCK, 0);
    Ok(out)
}

fn header(name: &str, size: usize, kind: u8, mode: u32) -> Result<[u8; BLOCK], VivariumError> {
    let (prefix, base) = split_name(name)?;
    let mut h = [0u8; BLOCK];
    h[..base.len()].copy_from_slice(base.as_bytes());
    put(&mut h[100..108], &format!("{mode:07o}"));
    put(&mut h[108..116], "0000000");
    put(&mut h[116..124], "0000000");
    put(&mut h[124..136], &format!("{size:011o}"));
    put(&mut h[136..148], "00000000000");
    h[148..156].fill(b' ');
    h[156] = kind;
    put(&mut h[257..263], "ustar");
    h[263..265].copy_from_slice(b"00");
    h[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    let sum: u32 = h.iter().map(|b| u32::from(*b)).sum();
    put(&mut h[148..156], &format!("{sum:06o}\0"));
    Ok(h)
}

fn put(field: &mut [u8], text: &str) {
    field[..text.len()].copy_from_slice(text.as_bytes());
}

/// Split a path into the ustar `(prefix, name)` pair: the name is at most 100
/// bytes, the prefix at most 155.
fn split_name(name: &str) -> Result<(&str, &str), VivariumError> {
    if name.len() <= 100 {
        return Ok(("", name));
    }
    let trimmed = name.trim_end_matches('/');
    for (i, _) in trimmed.match_indices('/') {
        let (prefix, rest) = (&name[..i], &name[i + 1..]);
        if prefix.len() <= 155 && rest.len() <= 100 {
            return Ok((prefix, rest));
        }
    }
    Err(VivariumError::Message(format!(
        "path too long for a tar header: {name}"
    )))
}

/// The regular files of a tar stream, in order. Directories and metadata
/// records are skipped.
///
/// # Errors
/// Returns an error if the stream is cut short or a size field is bad.
pub fn read(bytes: &[u8]) -> Result<Vec<TarFile>, VivariumError> {
    let mut files = Vec::new();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    while at < bytes.len() {
        if at + BLOCK > bytes.len() {
            if bytes[at..].iter().all(|b| *b == 0) {
                break;
            }
            return Err(VivariumError::Parse("tar stream is cut short".into()));
        }
        let block = &bytes[at..at + BLOCK];
        if block.iter().all(|b| *b == 0) {
            break;
        }
        let size = octal(&block[124..136])?;
        let start = at + BLOCK;
        let end = start + size;
        if end > bytes.len() {
            return Err(VivariumError::Parse("tar stream is cut short".into()));
        }
        let body = &bytes[start..end];
        match block[156] {
            b'L' => long_name = Some(cstr(body)),
            b'0' | 0 => {
                let name = long_name.take().unwrap_or_else(|| entry_name(block));
                files.push(TarFile {
                    path: name,
                    content: body.to_vec(),
                });
            }
            _ => long_name = None,
        }
        at = start + size.div_ceil(BLOCK) * BLOCK;
    }
    Ok(files)
}

fn entry_name(block: &[u8]) -> String {
    let name = cstr(&block[..100]);
    let ustar = &block[257..262] == b"ustar";
    let prefix = if ustar {
        cstr(&block[345..500])
    } else {
        String::new()
    };
    if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    }
}

fn cstr(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).into_owned()
}

fn octal(field: &[u8]) -> Result<usize, VivariumError> {
    let text = cstr(field);
    let digits = text.trim();
    if digits.is_empty() {
        return Ok(0);
    }
    usize::from_str_radix(digits, 8)
        .map_err(|_| VivariumError::Parse(format!("bad tar size field \"{digits}\"")))
}

#[cfg(test)]
#[path = "docker_tar_test.rs"]
mod tests;
