#![allow(dead_code)]

use std::io::ErrorKind;

use super::{base64::*, xml_helper::*};

pub struct XmlStreamReaderBlob {
    pub format:  String,
    pub name:    String,
    pub data:    Vec<u8>,
    pub dl_time: f64, // in seconds
}

pub enum XmlStreamReaderResult {
    BlobBegin {
        device_name: String,
        prop_name:   String,
        elem_name:   String,
        format:      String,
        len:         Option<usize>,
    },
    Xml {
        xml:   String,
        blobs: Vec<XmlStreamReaderBlob>,
    },
    TimeOut,
    Disconnected
}

#[derive(PartialEq)]
enum XmlStreamReaderState {
    Undefined,
    WaitForTag,
    WaitForTagEnd,
    WaitBlobVectorTag,
    WaitOneBlobTag,
    ReadingBlob,
    WaitOneBlobTagEnd,
}

pub struct XmlStreamReader {
    state:               XmlStreamReaderState,
    read_buffer:         Vec<u8>,
    stream_buffer:       Vec<u8>,
    buf_size:            usize,
    read_len:            usize,
    base64_decoder:      Base64Decoder,
    tag_re:              regex::bytes::Regex,
    tag_end_re:          regex::bytes::Regex,
    set_blob_vec_re:     regex::bytes::Regex,
    set_blob_vec_end_re: regex::bytes::Regex,
    one_blob_re:         regex::bytes::Regex,
    one_blob_end_re:     regex::bytes::Regex,
    blob_device:         String,
    blob_prop:           String,
    blob_elem:           String,
    blob_format:         String,
    blob_size:           Option<usize>,
    blob_dl_start:       std::time::Instant,
    blobs:               Vec<XmlStreamReaderBlob>,
    xml_text:            String,
}

impl XmlStreamReader {
    pub fn new() -> Self {
        Self {
            state:               XmlStreamReaderState::Undefined,
            read_buffer:         Vec::new(),
            stream_buffer:       Vec::new(),
            buf_size:            1024*32,
            read_len:            0,
            base64_decoder:      Base64Decoder::new(0),
            tag_re:              regex::bytes::Regex::new(r"<(\w+)[> /]").unwrap(),
            tag_end_re:          regex::bytes::Regex::new(r".").unwrap(),
            set_blob_vec_re:     regex::bytes::Regex::new(r"<setBLOBVector.*?>").unwrap(),
            set_blob_vec_end_re: regex::bytes::Regex::new(r"</setBLOBVector>").unwrap(),
            one_blob_re:         regex::bytes::Regex::new(r"^[^<]*<oneBLOB.*?>").unwrap(),
            one_blob_end_re:     regex::bytes::Regex::new(r"</oneBLOB>").unwrap(),
            blob_device:         String::new(),
            blob_prop:           String::new(),
            blob_elem:           String::new(),
            blob_format:         String::new(),
            blob_size:           None,
            blob_dl_start:       std::time::Instant::now(),
            blobs:               Vec::new(),
            xml_text:            String::new(),
        }
    }

    pub fn set_buf_size(&mut self, buf_size: usize) {
        self.buf_size = buf_size;
    }

    pub fn recover_after_error(&mut self) {
        self.read_buffer.clear();
        self.blobs.clear();
        self.base64_decoder.clear(0);
        self.state = XmlStreamReaderState::WaitForTag;
        self.read_len = 0;
    }

    pub fn receive_xml(
        &mut self,
        stream: &mut dyn std::io::Read
    ) -> anyhow::Result<XmlStreamReaderResult> {
        loop {
            match self.state {
                XmlStreamReaderState::Undefined => {
                    self.state = XmlStreamReaderState::WaitForTag;
                    continue;
                }
                XmlStreamReaderState::WaitForTag => {
                    self.read_buffer.extend_from_slice(&self.stream_buffer[..self.read_len]);
                    self.read_len = 0;
                    if let Some(re_res) = self.tag_re.captures(&self.read_buffer) {
                        self.xml_text.clear();
                        let tag_name = std::str::from_utf8(re_res.get(1).unwrap().as_bytes())?;
                        let end_tag_re_text = format!(r#"<{0}[^<>]*?/>|</{0}>"#, tag_name);
                        self.tag_end_re = regex::bytes::Regex::new(&end_tag_re_text)?;
                        if tag_name == "setBLOBVector" {
                            self.state = XmlStreamReaderState::WaitBlobVectorTag;
                        } else {
                            self.state = XmlStreamReaderState::WaitForTagEnd;
                        }
                        continue;
                    }
                }
                XmlStreamReaderState::WaitForTagEnd => {
                    self.read_buffer.extend_from_slice(&self.stream_buffer[..self.read_len]);
                    self.read_len = 0;
                    if let Some(re_res) = self.tag_end_re.captures(&self.read_buffer) {
                        let end_pos = re_res.get(0).unwrap().end();
                        let xml_text = std::str::from_utf8(&self.read_buffer[0..end_pos])?;
                        self.xml_text.push_str(xml_text);
                        self.read_buffer.drain(0..end_pos);
                        self.state = XmlStreamReaderState::WaitForTag;

                        return Ok(XmlStreamReaderResult::Xml {
                            xml:   std::mem::take(&mut self.xml_text),
                            blobs: std::mem::take(&mut self.blobs),
                        });
                    }
                }
                XmlStreamReaderState::WaitBlobVectorTag => {
                    self.read_buffer.extend_from_slice(&self.stream_buffer[..self.read_len]);
                    self.read_len = 0;
                    if let Some(re_res) = self.set_blob_vec_re.captures(&self.read_buffer) {
                        let end_pos = re_res.get(0).unwrap().end();
                        let xml_text = std::str::from_utf8(&self.read_buffer[0..end_pos])?;
                        self.xml_text.push_str(xml_text);
                        let mut xml_text = xml_text.trim().to_string();
                        self.read_buffer.drain(0..end_pos);
                        xml_text.push_str(r"</setBLOBVector>");
                        let mut xml_elem = xmltree::Element::parse(xml_text.as_bytes())?;
                        self.blob_device = xml_elem.attr_string_or_err("device")?;
                        self.blob_prop = xml_elem.attr_string_or_err("name")?;
                        self.state = XmlStreamReaderState::WaitOneBlobTag;
                        continue;
                    }
                }
                XmlStreamReaderState::WaitOneBlobTag => {
                    self.read_buffer.extend_from_slice(&self.stream_buffer[..self.read_len]);
                    if let Some(re_res) = self.one_blob_re.captures(&self.read_buffer) {
                        self.blob_dl_start = std::time::Instant::now();
                        let end_pos = re_res.get(0).unwrap().end();
                        let xml_text = std::str::from_utf8(&self.read_buffer[0..end_pos])?;
                        self.xml_text.push_str(xml_text);
                        let mut xml_text = xml_text.trim().to_string();
                        self.read_buffer.drain(0..end_pos);
                        xml_text.push_str(r"</oneBLOB>");
                        let mut xml_elem = xmltree::Element::parse(xml_text.as_bytes())?;
                        self.blob_elem = xml_elem.attr_string_or_err("name")?;
                        self.blob_format = xml_elem.attr_string_or_err("format")?;
                        let size = xml_elem.attributes.get("size").and_then(|attr| attr.parse::<usize>().ok());
                        let len = xml_elem.attributes.get("len").and_then(|attr| attr.parse::<usize>().ok());
                        self.blob_size = size.or(len);
                        self.base64_decoder.clear(usize::min(self.blob_size.unwrap_or_default(), 100_000_000));
                        self.stream_buffer.clear();
                        self.stream_buffer.extend_from_slice(&self.read_buffer);
                        self.read_buffer.clear();
                        self.read_len = self.stream_buffer.len();
                        self.state = XmlStreamReaderState::ReadingBlob;
                        return Ok(XmlStreamReaderResult::BlobBegin {
                            device_name: self.blob_device.clone(),
                            prop_name: self.blob_prop.clone(),
                            elem_name: self.blob_elem.clone(),
                            format:    self.blob_format.clone(),
                            len:       self.blob_size,
                        });
                    }
                    if self.set_blob_vec_end_re.find(&self.read_buffer).is_some() {
                        self.state = XmlStreamReaderState::WaitForTagEnd;
                        self.read_len = 0;
                        continue;
                    }
                }
                XmlStreamReaderState::ReadingBlob => {
                    let mut end_of_blob_found = false;
                    for &b in &self.stream_buffer[..self.read_len] {
                        if b != b'<' {
                            self.base64_decoder.add_byte(b);
                        } else {
                            end_of_blob_found = true;
                            break;
                        }
                    }
                    if end_of_blob_found {
                        let end_blob_pos = &self.stream_buffer[..self.read_len]
                            .iter()
                            .position(|b| *b == b'<')
                            .unwrap();
                        let blob_dl_time = self.blob_dl_start.elapsed().as_secs_f64();
                        self.read_buffer.extend_from_slice(&self.stream_buffer[*end_blob_pos..self.read_len]);
                        self.state = XmlStreamReaderState::WaitOneBlobTagEnd;
                        let blob = XmlStreamReaderBlob {
                            format:  self.blob_format.clone(),
                            name:    self.blob_elem.clone(),
                            data:    self.base64_decoder.take_result(),
                            dl_time: blob_dl_time,
                        };
                        self.blobs.push(blob);
                        self.read_len = 0;
                        continue;
                    }
                }
                XmlStreamReaderState::WaitOneBlobTagEnd => {
                    self.read_buffer.extend_from_slice(&self.stream_buffer[..self.read_len]);
                    self.read_len = 0;
                    if let Some(re_res) = self.one_blob_end_re.captures(&self.read_buffer) {
                        let end_pos = re_res.get(0).unwrap().end();
                        let xml_text = std::str::from_utf8(&self.read_buffer[0..end_pos])?;
                        self.xml_text.push_str(xml_text);
                        self.read_buffer.drain(0..end_pos);
                        self.state = XmlStreamReaderState::WaitOneBlobTag;
                        continue;
                    }
                }
            }

            self.stream_buffer.resize(self.buf_size, 0);
            let read_res = stream.read(&mut self.stream_buffer);
            self.read_len = match read_res {
                Err(e) => match e.kind() {
                    ErrorKind::NotConnected |
                    ErrorKind::ConnectionAborted |
                    ErrorKind::ConnectionReset =>
                        return Ok(XmlStreamReaderResult::Disconnected),
                    ErrorKind::TimedOut |
                    ErrorKind::WouldBlock =>
                        return Ok(XmlStreamReaderResult::TimeOut),
                    _ =>
                        return Err(e.into()),
                },
                Ok(0) =>
                    return Ok(XmlStreamReaderResult::Disconnected),
                Ok(size) =>
                    size,
            };
        }
    }

}


#[test]
fn test_reader_eof() {
    let mut reader = XmlStreamReader::new();
    let mut stream = std::io::Cursor::new("");

    let res = reader.receive_xml(&mut stream);
    assert!(matches!(res.unwrap(), XmlStreamReaderResult::Disconnected));
}

#[test]
fn test_reader_simple() {
    let do_test = |buf_size| {
        let mut reader = XmlStreamReader::new();
        reader.set_buf_size(buf_size);

        let mut test_simple_xml = |test_xml: &str| {
            let mut stream = std::io::Cursor::new(test_xml);
            let res = reader.receive_xml(&mut stream);
            let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else {
                panic!("Not XML");
            };
            assert_eq!(xml, test_xml);

            let res = reader.receive_xml(&mut stream);
            assert!(matches!(res.unwrap(), XmlStreamReaderResult::Disconnected));
        };

        test_simple_xml("<xml></xml>");
        test_simple_xml("<xml>1111</xml>");
        test_simple_xml("<xml><xml2>1111</xml2></xml>");
        test_simple_xml("<xml/>");
        test_simple_xml(r#"<xml arg1="aaa"/>"#);
        test_simple_xml(r#"<xml arg1="aaa" arg2 = "bbb"/>"#);

        let mut test_two_xml = |xml1: &str, xml2: &str| {
            let mut test_xml = xml1.to_string();
            test_xml.push_str(xml2);
            let mut stream = std::io::Cursor::new(test_xml);

            let res = reader.receive_xml(&mut stream);
            let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else {
                panic!("Not XML");
            };
            assert_eq!(xml, xml1);

            let res = reader.receive_xml(&mut stream);
            let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else {
                panic!("Not XML");
            };
            assert_eq!(xml, xml2);

            let res = reader.receive_xml(&mut stream);
            assert!(matches!(res.unwrap(), XmlStreamReaderResult::Disconnected));
        };

        test_two_xml("<xml/>",      "<xml2/>");
        test_two_xml("<xml></xml>", "<xml2/>");
        test_two_xml("<xml2/>",     "<xml></xml>");
        test_two_xml("<xml></xml>", "<xml></xml>");
        test_two_xml("<xml></xml>", "<xml2></xml2>");
    };

    for buf_size in 1..100 {
        do_test(buf_size);
    }
    do_test(100);
    do_test(1000);
    do_test(10000);
}

#[test]
fn test_reader() {
    let do_test = |buf_size| {
        let mut reader = XmlStreamReader::new();
        reader.set_buf_size(buf_size);

        let mut stream = std::io::Cursor::new(r#"
            <xml1/>
            <xml2></xml2>
            <setBLOBVector device="CCD Simulator" name="CCD1" state="Ok" timeout="60" timestamp="2023-06-03T19:31:34">
                <oneBLOB name="CCD1" size="8" format=".text1" len="8">dGVzdHRlc3Q=</oneBLOB>
                <oneBLOB name="CCD2" size="6" format=".text2" len="6">YmxhYmxh</oneBLOB>
            </setBLOBVector>
            <setBLOBVector device="TestDev" name="Test1" state="Ok" timeout="60" timestamp="2023-06-03T19:31:34">
                <oneBLOB name="CCD1" size="6" format=".text3" len="6">bGFsYWxh</oneBLOB>
            </setBLOBVector>
            <xml3></xml3>
        "#);

        // xml1

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else { panic!("Not XML"); };
        assert_eq!(xml.trim(), "<xml1/>");

        // xml2

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else { panic!("Not XML"); };
        assert_eq!(xml.trim(), "<xml2></xml2>");

        // Blob-1 start

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::BlobBegin { device_name, prop_name, elem_name, format, len } = res.unwrap() else {
            panic!("Not Blob begin");
        };
        assert_eq!(device_name, "CCD Simulator");
        assert_eq!(prop_name,   "CCD1");
        assert_eq!(elem_name,   "CCD1");
        assert_eq!(format,      ".text1");
        assert_eq!(len,         Some(8));

        // Blob-2 start

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::BlobBegin { device_name, prop_name, elem_name, format, len } = res.unwrap() else {
            panic!("Not Blob begin");
        };
        assert_eq!(device_name, "CCD Simulator");
        assert_eq!(prop_name,   "CCD1");
        assert_eq!(elem_name,   "CCD2");
        assert_eq!(format,      ".text2");
        assert_eq!(len,         Some(6));

        // XML + Blobs

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::Xml { blobs, .. } = res.unwrap() else {
            panic!("Not XML");
        };
        assert_eq!(blobs.len(), 2);
        let blob1 = &blobs[0];
        assert_eq!(blob1.data.as_slice(), b"testtest");
        assert_eq!(blob1.format, ".text1");
        let blob2 = &blobs[1];
        assert_eq!(blob2.data.as_slice(), b"blabla");
        assert_eq!(blob2.format, ".text2");

        // Blob-3 start

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::BlobBegin { device_name, prop_name, elem_name, format, len } = res.unwrap() else {
            panic!("Not Blob begin");
        };
        assert_eq!(device_name, "TestDev");
        assert_eq!(prop_name,   "Test1");
        assert_eq!(elem_name,   "CCD1");
        assert_eq!(format,      ".text3");
        assert_eq!(len,         Some(6));

        // XML + Blob3

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::Xml { blobs, .. } = res.unwrap() else {
            panic!("Not XML");
        };
        assert_eq!(blobs.len(), 1);
        let blob1 = &blobs[0];
        assert_eq!(blob1.data.as_slice(), b"lalala");
        assert_eq!(blob1.format, ".text3");

        // xml3

        let res = reader.receive_xml(&mut stream);
        let XmlStreamReaderResult::Xml { xml, .. } = res.unwrap() else { panic!("Not XML"); };
        assert_eq!(xml.trim(), "<xml3></xml3>");

        // End of stream

        let res = reader.receive_xml(&mut stream);
        assert!(matches!(res.unwrap(), XmlStreamReaderResult::Disconnected));
    };

    for buf_size in 1..100 {
        do_test(buf_size);
    }
    do_test(100);
    do_test(1000);
    do_test(10000);
}