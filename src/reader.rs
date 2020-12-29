// reader.rs

use std::io::BufRead;
use std::io::BufReader;
use std::fs::File;
use std::io::Seek;
use std::io::SeekFrom;

/// @brief Consumes comments lines from the stream. File cursor is left right
/// after comments.
pub fn consume_comments2(file_reader: &mut BufReader<File>) -> std::io::Result<Vec<String>> {
  let mut buf_str = String::new();
  let mut res = Vec::<String>::new();
  let mut comments_bytes_count: u64 = 0;
  loop {
    let read_bytes_count: usize = file_reader.read_line(&mut buf_str)?;
    if buf_str.starts_with('#') {
      res.push(String::from(&buf_str[1..buf_str.len() - 1]));
    } else {
      // read_line returns Ok(0) when reached EOF.
      if read_bytes_count == 0 {
        return Err(std::io::Error::new(
          std::io::ErrorKind::InvalidInput,
          "File is empty.",
        ));
      }
      file_reader.seek(SeekFrom::Start(comments_bytes_count))?;
      return Ok(res);
    }
    buf_str.clear();
    comments_bytes_count += read_bytes_count as u64;
  }
}
