//! What the Rust port needs of Python's behaviour, so its messages and hashes are Python's:
//! repr() of TOML values, json.dumps(..., sort_keys=True), fnmatch.fnmatchcase,
//! difflib.get_close_matches (cutoff and ties as Python's), and SHA-256.

use toml::Value;

use crate::policy::py_repr;

/// Python's repr() of a float (the shortest digits that round-trip; exponent form below
/// 1e-4 and from 1e16 on, as Python's 'r' format).
pub fn float_repr(f: f64) -> String {
    if f.is_nan() {
        return "nan".into();
    }
    if f.is_infinite() {
        return if f > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let e = format!("{f:e}"); // "-1.5e20", "0e0"
    let (sign, e) = e.strip_prefix('-').map_or(("", e.as_str()), |r| ("-", r));
    let (mant, exp) = e.split_once('e').unwrap_or((e, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let digits = if digits.trim_start_matches('0').is_empty() { "0".to_string() } else { digits };
    if (-4..16).contains(&exp) {
        let decpt = exp + 1;
        let body = if decpt <= 0 {
            format!("0.{}{digits}", "0".repeat((-decpt) as usize))
        } else if decpt as usize >= digits.len() {
            format!("{digits}{}.0", "0".repeat(decpt as usize - digits.len()))
        } else {
            format!("{}.{}", &digits[..decpt as usize], &digits[decpt as usize..])
        };
        format!("{sign}{body}")
    } else {
        let rest = if digits.len() > 1 { format!(".{}", &digits[1..]) } else { String::new() };
        format!("{sign}{}{rest}e{}{:02}", &digits[..1], if exp < 0 { '-' } else { '+' }, exp.abs())
    }
}

/// Python's repr() of a value tomllib gives (datetimes only approximately).
pub fn repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr(s),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => float_repr(*f),
        Value::Boolean(b) => if *b { "True".into() } else { "False".into() },
        Value::Datetime(d) => format!("datetime({d})"),
        Value::Array(a) => format!("[{}]", a.iter().map(repr).collect::<Vec<_>>().join(", ")),
        Value::Table(t) => format!("{{{}}}", t.iter().map(|(k, v)| format!("{}: {}", py_repr(k), repr(v)))
            .collect::<Vec<_>>().join(", ")),
    }
}

/// Python's repr() of a list of strings.
pub fn repr_strs<S: AsRef<str>>(v: &[S]) -> String {
    format!("[{}]", v.iter().map(|s| py_repr(s.as_ref())).collect::<Vec<_>>().join(", "))
}

fn json_str(s: &str, out: &mut String) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 || (c as u32) > 0x7e => {
                let mut buf = [0u16; 2];
                for u in c.encode_utf16(&mut buf) {
                    out.push_str(&format!("\\u{u:04x}"));
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

/// json.dumps(v, sort_keys=True) with Python's defaults (", " and ": ", ensure_ascii).
pub fn json_dumps(v: &Value, out: &mut String) {
    json_dumps_by(v, out, true)
}

/// json.dumps(v) of a TOML value, keys in their own order.
pub fn json_dumps_unsorted(v: &Value) -> String {
    let mut out = String::new();
    json_dumps_by(v, &mut out, false);
    out
}

/// json.dumps(v) of a JSON value as Python writes it (", ", ": ", ensure_ascii, keys in order).
pub fn py_json(v: &serde_json::Value) -> String {
    let mut out = String::new();
    py_json_into(v, &mut out);
    out
}

fn py_json_into(v: &serde_json::Value, out: &mut String) {
    use serde_json::Value as J;
    match v {
        J::Null => out.push_str("null"),
        J::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        J::Number(n) => match (n.as_i64(), n.as_u64(), n.as_f64()) {
            (Some(i), _, _) => out.push_str(&i.to_string()),
            (_, Some(u), _) => out.push_str(&u.to_string()),
            (_, _, Some(f)) => out.push_str(&float_repr(f)),
            _ => out.push_str(&n.to_string()),
        },
        J::String(s) => json_str(s, out),
        J::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                py_json_into(x, out);
            }
            out.push(']');
        }
        J::Object(m) => {
            out.push('{');
            for (i, (k, x)) in m.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_str(k, out);
                out.push_str(": ");
                py_json_into(x, out);
            }
            out.push('}');
        }
    }
}

fn json_dumps_by(v: &Value, out: &mut String, sort: bool) {
    match v {
        Value::String(s) => json_str(s, out),
        Value::Integer(i) => out.push_str(&i.to_string()),
        Value::Float(f) if f.is_nan() => out.push_str("NaN"),
        Value::Float(f) if f.is_infinite() => out.push_str(if *f > 0.0 { "Infinity" } else { "-Infinity" }),
        Value::Float(f) => out.push_str(&float_repr(*f)),
        Value::Boolean(b) => out.push_str(if *b { "true" } else { "false" }),
        Value::Datetime(d) => json_str(&d.to_string(), out), // (Python cannot dump these at all)
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_dumps_by(x, out, sort);
            }
            out.push(']');
        }
        Value::Table(t) => {
            let mut keys: Vec<&String> = t.keys().collect();
            if sort {
                keys.sort(); // code point order, as Python's sort of str
            }
            out.push('{');
            for (i, k) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                json_str(k, out);
                out.push_str(": ");
                json_dumps_by(&t[k], out, sort);
            }
            out.push('}');
        }
    }
}

/// fnmatch.fnmatchcase: `*`, `?`, `[seq]`, `[!seq]` (an unclosed `[` is itself).
pub fn fnmatchcase(name: &str, pat: &str) -> bool {
    let n: Vec<char> = name.chars().collect();
    let p: Vec<char> = pat.chars().collect();
    fn class(p: &[char], i: usize, c: char) -> Option<(bool, usize)> {
        // p[i] == '['; Some((matches, index after the class)) or None when not a class
        let mut j = i + 1;
        let neg = j < p.len() && p[j] == '!';
        if neg {
            j += 1;
        }
        if j < p.len() && p[j] == ']' {
            j += 1; // a leading ] is literal
        }
        while j < p.len() && p[j] != ']' {
            j += 1;
        }
        if j >= p.len() {
            return None;
        }
        let body = &p[i + 1 + usize::from(neg)..j];
        let mut hit = false;
        let mut k = 0;
        while k < body.len() {
            if k + 2 < body.len() && body[k + 1] == '-' {
                if body[k] <= c && c <= body[k + 2] {
                    hit = true;
                }
                k += 3;
            } else {
                if body[k] == c {
                    hit = true;
                }
                k += 1;
            }
        }
        Some((hit != neg, j + 1))
    }
    fn go(n: &[char], p: &[char]) -> bool {
        let (mut ni, mut pi) = (0, 0);
        let mut star: Option<(usize, usize)> = None;
        loop {
            if pi < p.len() {
                match p[pi] {
                    '*' => {
                        star = Some((pi, ni));
                        pi += 1;
                        continue;
                    }
                    '?' if ni < n.len() => {
                        ni += 1;
                        pi += 1;
                        continue;
                    }
                    '[' if ni < n.len() => match class(p, pi, n[ni]) {
                        Some((true, next)) => {
                            ni += 1;
                            pi = next;
                            continue;
                        }
                        Some((false, _)) => {}
                        None if p[pi] == n[ni] => {
                            ni += 1;
                            pi += 1;
                            continue;
                        }
                        None => {}
                    },
                    c if ni < n.len() && c == n[ni] && c != '?' && c != '[' => {
                        ni += 1;
                        pi += 1;
                        continue;
                    }
                    _ => {}
                }
            } else if ni == n.len() {
                return true;
            }
            match star {
                Some((sp, sn)) if sn < n.len() => {
                    star = Some((sp, sn + 1));
                    pi = sp + 1;
                    ni = sn + 1;
                }
                _ => return false,
            }
        }
    }
    go(&n, &p)
}

/// difflib.SequenceMatcher(None, a, b).ratio(), on code points.
pub fn ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let t = a.len() + b.len();
    if t == 0 {
        return 1.0;
    }
    // b2j: where each element of b is; popular ones dropped when b has 200 or more (autojunk)
    let mut b2j: std::collections::HashMap<char, Vec<usize>> = std::collections::HashMap::new();
    for (j, c) in b.iter().enumerate() {
        b2j.entry(*c).or_default().push(j);
    }
    if b.len() >= 200 {
        let ntest = b.len() / 100 + 1;
        b2j.retain(|_, v| v.len() <= ntest);
    }
    let longest = |alo: usize, ahi: usize, blo: usize, bhi: usize| -> (usize, usize, usize) {
        let (mut besti, mut bestj, mut bestsize) = (alo, blo, 0usize);
        let mut j2len: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
        for (i, ai) in a.iter().enumerate().take(ahi).skip(alo) {
            let mut new: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
            if let Some(js) = b2j.get(ai) {
                for &j in js {
                    if j < blo {
                        continue;
                    }
                    if j >= bhi {
                        break;
                    }
                    let k = j.checked_sub(1).and_then(|p| j2len.get(&p)).copied().unwrap_or(0) + 1;
                    new.insert(j, k);
                    if k > bestsize {
                        (besti, bestj, bestsize) = (i + 1 - k, j + 1 - k, k);
                    }
                }
            }
            j2len = new;
        }
        while besti > alo && bestj > blo && a[besti - 1] == b[bestj - 1] {
            (besti, bestj, bestsize) = (besti - 1, bestj - 1, bestsize + 1);
        }
        while besti + bestsize < ahi && bestj + bestsize < bhi && a[besti + bestsize] == b[bestj + bestsize] {
            bestsize += 1;
        }
        (besti, bestj, bestsize)
    };
    let mut matches = 0;
    let mut queue = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = queue.pop() {
        let (i, j, k) = longest(alo, ahi, blo, bhi);
        if k > 0 {
            matches += k;
            if alo < i && blo < j {
                queue.push((alo, i, blo, j));
            }
            if i + k < ahi && j + k < bhi {
                queue.push((i + k, ahi, j + k, bhi));
            }
        }
    }
    2.0 * matches as f64 / t as f64
}

/// difflib.get_close_matches(word, possibilities, n=1, cutoff): the best one (ties: the
/// greater string, as heapq.nlargest on (score, x)).
pub fn close_match<'a>(word: &str, possibilities: impl IntoIterator<Item = &'a str>, cutoff: f64) -> Option<&'a str> {
    let mut best: Option<(f64, &str)> = None;
    for x in possibilities {
        let r = ratio(x, word);
        if r >= cutoff && best.is_none_or(|(br, bx)| r > br || (r == br && x > bx)) {
            best = Some((r, x));
        }
    }
    best.map(|(_, x)| x)
}

/// SHA-256, hex.
pub fn sha256_hex(data: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98,
        0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
        0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8,
        0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
        0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819,
        0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
        0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] =
        [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    let mut msg = data.to_vec();
    let bits = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bits.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for (i, word) in chunk.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = h;
        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            (hh, g, f, e, d, c, b, a) = (g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2));
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e, f, g, hh]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats() {
        for (f, want) in [(1.0, "1.0"), (0.1, "0.1"), (1e16, "1e+16"), (1e15, "1000000000000000.0"),
                          (1e-5, "1e-05"), (0.0001, "0.0001"), (-0.0, "-0.0"), (1.5e300, "1.5e+300"),
                          (123.456, "123.456"), (0.0, "0.0"), (f64::INFINITY, "inf")] {
            assert_eq!(float_repr(f), want, "{f}");
        }
    }

    #[test]
    fn reprs() {
        let v: toml::Table = "a = [\"x\", 1, true, 1.5]\nb = { path = \"~/p\", mode = \"rw\" }".parse().unwrap();
        assert_eq!(repr(&v["a"]), "['x', 1, True, 1.5]");
        assert_eq!(repr(&v["b"]), "{'path': '~/p', 'mode': 'rw'}");
    }

    #[test]
    fn dumps() {
        let v: toml::Table = "z = 1\na = [\"é\", {y = 2, x = \"q\\\"\"}]\nm = 1.0".parse().unwrap();
        let mut out = String::new();
        json_dumps(&Value::Table(v), &mut out);
        assert_eq!(out, r#"{"a": ["\u00e9", {"x": "q\"", "y": 2}], "m": 1.0, "z": 1}"#);
    }

    #[test]
    fn globs() {
        for (n, p, want) in [("org.freedesktop.systemd1", "org.freedesktop.systemd1*", true),
                             ("org.freedesktop.systemd", "org.freedesktop.systemd1*", false),
                             ("SSH_AUTH_SOCK", "SSH_AUTH_SOCK", true), ("MY_TOKEN_X", "*TOKEN*", true),
                             ("abc", "a?c", true), ("abc", "a[bx]c", true), ("abc", "a[!b]c", false),
                             ("a[c", "a[c", true), ("org.gtk.vfs", "org.gtk.vfs*", true), ("", "*", true),
                             ("org.x", "org.*", true), ("o", "org.*", false)] {
            assert_eq!(fnmatchcase(n, p), want, "{n} {p}");
        }
    }

    #[test]
    fn close_matches() {
        // values from Python's difflib
        assert!((ratio("network", "netwrok") - 0.857142857142857).abs() < 1e-12);
        assert_eq!(close_match("netwrok", ["network", "netbird", "dns"], 0.75), Some("network"));
        assert_eq!(close_match("sandwich", ["network", "netbird", "dns"], 0.75), None);
        assert!((ratio("abcd", "bcde") - 0.75).abs() < 1e-12);
    }

    #[test]
    fn sha() {
        assert_eq!(sha256_hex(b""), "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855");
        assert_eq!(sha256_hex(b"abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
        assert_eq!(sha256_hex(&[b'a'; 1000]), "41edece42d63e8d9bf515a9ba6932e1c20cbc9f5a5d134645adb5db1b9737ea3");
    }
}
