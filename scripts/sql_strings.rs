pub fn strip_sql_strings(s: &str) -> String {
  let mut result = String::with_capacity(s.len());
  let mut in_string = false;
  let mut chars = s.chars().peekable();
  while let Some(ch) = chars.next() {
    match ch {
      '\'' if !in_string => {
        in_string = true;
        result.push(' ');
      }
      '\'' if in_string => {
        if chars.peek() == Some(&'\'') {
          chars.next();
          result.push(' ');
          result.push(' ');
        } else {
          in_string = false;
          result.push(' ');
        }
      }
      _ if in_string => result.push(' '),
      _ => result.push(ch),
    }
  }
  result
}
