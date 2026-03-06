use similar::{ChangeTag, TextDiff};

pub fn colorize_unified_diff(diff: &str, original: &str, modified: &str) -> String {
    let lines: Vec<&str> = diff.lines().collect();
    let mut result = String::new();

    let line_diff = TextDiff::from_lines(original, modified);
    let changes: Vec<_> = line_diff.iter_all_changes().collect();

    let mut colored_del: Vec<String> = Vec::new();
    let mut colored_ins: Vec<String> = Vec::new();

    let mut i = 0;
    while i < changes.len() {
        match changes[i].tag() {
            ChangeTag::Equal => { i += 1; }
            ChangeTag::Delete => {
                let del_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Delete { i += 1; }
                let ins_start = i;
                while i < changes.len() && changes[i].tag() == ChangeTag::Insert { i += 1; }

                let dels = &changes[del_start..ins_start];
                let inss = &changes[ins_start..i];
                let pair_count = dels.len().min(inss.len());

                for j in 0..pair_count {
                    let old_val = dels[j].value().trim_end_matches('\n');
                    let new_val = inss[j].value().trim_end_matches('\n');
                    let char_diff = TextDiff::from_chars(old_val, new_val);

                    let mut old_buf = String::new();
                    let mut new_buf = String::new();
                    for c in char_diff.iter_all_changes() {
                        match c.tag() {
                            ChangeTag::Equal => {
                                old_buf += c.value();
                                new_buf += c.value();
                            }
                            ChangeTag::Delete => {
                                old_buf += "\x1b[31m";
                                old_buf += c.value();
                                old_buf += "\x1b[0m";
                            }
                            ChangeTag::Insert => {
                                new_buf += "\x1b[32m";
                                new_buf += c.value();
                                new_buf += "\x1b[0m";
                            }
                        }
                    }
                    colored_del.push(format!("-{old_buf}"));
                    colored_ins.push(format!("+{new_buf}"));
                }
                for j in pair_count..dels.len() {
                    colored_del.push(format!(
                        "-\x1b[31m{}\x1b[0m",
                        dels[j].value().trim_end_matches('\n')
                    ));
                }
                for j in pair_count..inss.len() {
                    colored_ins.push(format!(
                        "+\x1b[32m{}\x1b[0m",
                        inss[j].value().trim_end_matches('\n')
                    ));
                }
            }
            ChangeTag::Insert => {
                colored_ins.push(format!(
                    "+\x1b[32m{}\x1b[0m",
                    changes[i].value().trim_end_matches('\n')
                ));
                i += 1;
            }
        }
    }

    let mut del_idx = 0;
    let mut ins_idx = 0;
    for line in &lines {
        if line.starts_with('-') && !line.starts_with("---") {
            if del_idx < colored_del.len() {
                result += &colored_del[del_idx];
                del_idx += 1;
            } else {
                result += line;
            }
        } else if line.starts_with('+') && !line.starts_with("+++") {
            if ins_idx < colored_ins.len() {
                result += &colored_ins[ins_idx];
                ins_idx += 1;
            } else {
                result += line;
            }
        } else {
            result += line;
        }
        result += "\n";
    }

    result.trim_end().to_string()
}
