// Copyright 2015 The Rust Project Developers. See the COPYRIGHT
// file at the top-level directory of this distribution and at
// http://rust-lang.org/COPYRIGHT.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// http://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or http://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.


// TODO
// print to files
// tests

use strings::string_buffer::StringBuffer;
use std::collections::HashMap;
use syntax::codemap::{CodeMap, Span, BytePos};
use std::fmt;
use std::fs::File;
use std::io::{Write, stdout};
use WriteMode;
use NewlineStyle;

// This is basically a wrapper around a bunch of Ropes which makes it convenient
// to work with libsyntax. It is badly named.
pub struct ChangeSet<'a> {
    file_map: HashMap<String, StringBuffer>,
    codemap: &'a CodeMap,
    file_spans: Vec<(u32, u32)>,
}

impl<'a> ChangeSet<'a> {
    // Create a new ChangeSet for a given libsyntax CodeMap.
    pub fn from_codemap(codemap: &'a CodeMap) -> ChangeSet<'a> {
        let mut result = ChangeSet {
            file_map: HashMap::new(),
            codemap: codemap,
            file_spans: Vec::with_capacity(codemap.files.borrow().len()),
        };

        for f in codemap.files.borrow().iter() {
            // Use the length of the file as a heuristic for how much space we
            // need. I hope that at some stage someone rounds this up to the next
            // power of two. TODO check that or do it here.
            result.file_map.insert(f.name.clone(),
                                   StringBuffer::with_capacity(f.src.as_ref().unwrap().len()));

            result.file_spans.push((f.start_pos.0, f.end_pos.0));
        }

        result.file_spans.sort();

        result
    }

    pub fn filespans_for_span(&self, start: BytePos, end: BytePos) -> Vec<(u32, u32)> {
        assert!(start.0 <= end.0);

        if self.file_spans.len() == 0 {
            return Vec::new();
        }

        // idx is the index into file_spans which indicates the current file, we
        // with the file start denotes.
        let mut idx = match self.file_spans.binary_search(&(start.0, ::std::u32::MAX)) {
            Ok(i) => i,
            Err(0) => 0,
            Err(i) => i - 1,
        };

        let mut result = Vec::new();
        let mut start = start.0;
        loop {
            let cur_file = &self.file_spans[idx];
            idx += 1;

            if idx >= self.file_spans.len() || start >= end.0 {
                if start < end.0 {
                    result.push((start, end.0));
                }
                return result;
            }

            let end = ::std::cmp::min(cur_file.1 - 1, end.0);
            if start < end {
                result.push((start, end));
            }
            start = self.file_spans[idx].0;
        }
    }

    pub fn push_str(&mut self, filename: &str, text: &str) {
        let buf = self.file_map.get_mut(&*filename).unwrap();
        buf.push_str(text)
    }

    pub fn push_str_span(&mut self, span: Span, text: &str) {
        let file_name = self.codemap.span_to_filename(span);
        self.push_str(&file_name, text)
    }

    pub fn get_mut(&mut self, file_name: &str) -> &mut StringBuffer {
        self.file_map.get_mut(file_name).unwrap()
    }

    pub fn cur_offset(&mut self, filename: &str) -> usize {
        self.file_map[&*filename].cur_offset()
    }

    pub fn cur_offset_span(&mut self, span: Span) -> usize {
        let filename = self.codemap.span_to_filename(span);
        self.cur_offset(&filename)
    }

    // Return an iterator over the entire changed text.
    pub fn text<'c>(&'c self) -> FileIterator<'c, 'a> {
        FileIterator {
            change_set: self,
            keys: self.file_map.keys().collect(),
            cur_key: 0,
        }
    }

    // Append a newline to the end of each file.
    pub fn append_newlines(&mut self) {
        for (_, s) in self.file_map.iter_mut() {
            s.push_str("\n");
        }
    }

    pub fn write_all_files(&self,
                           mode: WriteMode)
                           -> Result<(HashMap<String, String>), ::std::io::Error> {
        let mut result = HashMap::new();
        for filename in self.file_map.keys() {
            let one_result = try!(self.write_file(filename, mode));
            if let Some(r) = one_result {
                result.insert(filename.clone(), r);
            }
        }

        Ok(result)
    }

    pub fn write_file(&self,
                      filename: &str,
                      mode: WriteMode)
                      -> Result<Option<String>, ::std::io::Error> {
        let text = &self.file_map[filename];

        // prints all newlines either as `\n` or as `\r\n`
        fn write_system_newlines<T>(
            mut writer: T,
            text: &StringBuffer)
            -> Result<(), ::std::io::Error>
            where T: Write,
        {
            match config!(newline_style) {
                NewlineStyle::Unix => write!(writer, "{}", text),
                NewlineStyle::Windows => {
                    for (c, _) in text.chars() {
                        match c {
                            '\n' => try!(write!(writer, "\r\n")),
                            '\r' => continue,
                            c => try!(write!(writer, "{}", c)),
                        }
                    }
                    Ok(())
                },
            }
        }

        match mode {
            WriteMode::Overwrite => {
                // Do a little dance to make writing safer - write to a temp file
                // rename the original to a .bk, then rename the temp file to the
                // original.
                let tmp_name = filename.to_owned() + ".tmp";
                let bk_name = filename.to_owned() + ".bk";
                {
                    // Write text to temp file
                    let tmp_file = try!(File::create(&tmp_name));
                    try!(write_system_newlines(tmp_file, text));
                }

                try!(::std::fs::rename(filename, bk_name));
                try!(::std::fs::rename(tmp_name, filename));
            }
            WriteMode::NewFile(extn) => {
                let filename = filename.to_owned() + "." + extn;
                let file = try!(File::create(&filename));
                try!(write_system_newlines(file, text));
            }
            WriteMode::Display => {
                println!("{}:\n", filename);
                let stdout = stdout();
                let stdout_lock = stdout.lock();
                try!(write_system_newlines(stdout_lock, text));
            }
            WriteMode::Return(_) => {
                // io::Write is not implemented for String, working around with Vec<u8>
                let mut v = Vec::new();
                try!(write_system_newlines(&mut v, text));
                // won't panic, we are writing correct utf8
                return Ok(Some(String::from_utf8(v).unwrap()));
            }
        }

        Ok(None)
    }
}

// Iterates over each file in the ChangSet. Yields the filename and the changed
// text for that file.
pub struct FileIterator<'c, 'a: 'c> {
    change_set: &'c ChangeSet<'a>,
    keys: Vec<&'c String>,
    cur_key: usize,
}

impl<'c, 'a> Iterator for FileIterator<'c, 'a> {
    type Item = (&'c str, &'c StringBuffer);

    fn next(&mut self) -> Option<(&'c str, &'c StringBuffer)> {
        if self.cur_key >= self.keys.len() {
            return None;
        }

        let key = self.keys[self.cur_key];
        self.cur_key += 1;
        return Some((&key, &self.change_set.file_map[&*key]))
    }
}

impl<'a> fmt::Display for ChangeSet<'a> {
    // Prints the entire changed text.
    fn fmt(&self, fmt: &mut fmt::Formatter) -> Result<(), fmt::Error> {
        for (f, r) in self.text() {
            try!(write!(fmt, "{}:\n", f));
            try!(write!(fmt, "{}\n\n", r));
        }
        Ok(())
    }
}
