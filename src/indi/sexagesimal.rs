pub fn value_to_sexagesimal(value: f64, zero: bool, frac: u8) -> String {
    let sign = if value < 0.0 { "-" } else { "" };
    let value = value.abs();
    let mut hours = value.trunc() as i32;
    let round = match frac {
        9 => 0.5,
        8 => 5.0,
        6 => 50.0,
        5 => 50.0 * 60.0 / 10.0,
        3 => 50.0 * 60.0,
        _ => 0.0,
    };
    let mut seconds100 = (value.fract() * 3600.0 * 100.0 + round) as u32;
    if seconds100 >= 3600 * 100 {
        hours += if hours < 0 { -1 } else { 1 };
        seconds100 -= 3600 * 100;
    }
    let minutes100 = seconds100 / 60;
    seconds100 %= 60 * 100;
    match (frac, zero) {
        (3, false) => format!("{}{}:{:02}", sign, hours, minutes100 / 100),
        (3, true)  => format!("{}{:02}:{:02}", sign, hours, minutes100 / 100),
        (5, false) => format!("{}{}:{:02}.{}", sign, hours, minutes100 / 100, (minutes100 % 100)/10),
        (5, true)  => format!("{}{:02}:{:02}.{}", sign, hours, minutes100 / 100, (minutes100 % 100)/10),
        (6, false) => format!("{}{}:{:02}:{:02}", sign, hours, minutes100 / 100, seconds100 / 100),
        (6, true)  => format!("{}{:02}:{:02}:{:02}", sign, hours, minutes100 / 100, seconds100 / 100),
        (8, false) => format!("{}{}:{:02}:{:02}.{}", sign, hours, minutes100 / 100, seconds100 / 100, (seconds100 % 100) / 10),
        (8, true)  => format!("{}{:02}:{:02}:{:02}.{}", sign, hours, minutes100 / 100, seconds100 / 100, (seconds100 % 100) / 10),
        (9, false) => format!("{}{}:{:02}:{:02}.{:02}", sign, hours, minutes100 / 100, seconds100 / 100, seconds100 % 100),
        (9, true)  => format!("{}{:02}:{:02}:{:02}.{:02}", sign, hours, minutes100 / 100, seconds100 / 100, seconds100 % 100),
        _          => value.to_string(),
    }
}

pub fn sexagesimal_to_value(text: &str) -> Option<f64> {
    use once_cell::sync::OnceCell;
    let text = text.trim();

    // -00:00:00.00
    static F9_RE: OnceCell<regex::Regex> = OnceCell::new();
    let f9_re = F9_RE.get_or_init(|| {
        regex::Regex::new(r"([+-]?)(\d+):(\d+):(\d+)\.(\d\d)").unwrap()
    });
    if let Some(res) = f9_re.captures(text) {
        let is_neg = &res[1] == "-";
        let hours = res[2].parse::<f64>().unwrap_or(0.0);
        let minutes = res[3].parse::<f64>().unwrap_or(0.0);
        let seconds = res[4].parse::<f64>().unwrap_or(0.0) +
                      res[5].parse::<f64>().unwrap_or(0.0) / 100.0;
        let value = hours + minutes / 60.0 + seconds / 3600.0;
        return Some(if !is_neg {value} else {-value});
    }

    // -00:00:00.0
    static F8_RE: OnceCell<regex::Regex> = OnceCell::new();
    let f8_re = F8_RE.get_or_init(|| {
        regex::Regex::new(r"([+-]?)(\d+):(\d+):(\d+)\.(\d)").unwrap()
    });
    if let Some(res) = f8_re.captures(text) {
        let is_neg = &res[1] == "-";
        let hours = res[2].parse::<f64>().unwrap_or(0.0);
        let minutes = res[3].parse::<f64>().unwrap_or(0.0);
        let seconds = res[4].parse::<f64>().unwrap_or(0.0) +
                      res[5].parse::<f64>().unwrap_or(0.0) / 10.0;
        let value = hours + minutes / 60.0 + seconds / 3600.0;
        return Some(if !is_neg {value} else {-value});
    }

    // -00:00:00
    static F6_RE: OnceCell<regex::Regex> = OnceCell::new();
    let f6_re = F6_RE.get_or_init(|| {
        regex::Regex::new(r"([+-]?)(\d+):(\d+):(\d+)").unwrap()
    });
    if let Some(res) = f6_re.captures(text) {
        let is_neg = &res[1] == "-";
        let hours = res[2].parse::<f64>().unwrap_or(0.0);
        let minutes = res[3].parse::<f64>().unwrap_or(0.0);
        let seconds = res[4].parse::<f64>().unwrap_or(0.0);
        let value = hours + minutes / 60.0 + seconds / 3600.0;
        return Some(if !is_neg {value} else {-value});
    }

    // -00:00.0
    static F5_RE: OnceCell<regex::Regex> = OnceCell::new();
    let f5_re = F5_RE.get_or_init(|| {
        regex::Regex::new(r"([+-]?)(\d+):(\d+)\.(\d)").unwrap()
    });
    if let Some(res) = f5_re.captures(text) {
        let is_neg = &res[1] == "-";
        let hours = res[2].parse::<f64>().unwrap_or(0.0);
        let minutes = res[3].parse::<f64>().unwrap_or(0.0) +
                      res[4].parse::<f64>().unwrap_or(0.0) / 10.0;
        let value = hours + minutes / 60.0;
        return Some(if !is_neg {value} else {-value});
    }

    // -00:00
    static F3_RE: OnceCell<regex::Regex> = OnceCell::new();
    let f3_re = F3_RE.get_or_init(|| {
        regex::Regex::new(r"([+-]?)(\d+):(\d+)").unwrap()
    });
    if let Some(res) = f3_re.captures(text) {
        let is_neg = &res[1] == "-";
        let int = res[2].parse::<f64>().unwrap_or(0.0);
        let frac1 = res[3].parse::<f64>().unwrap_or(0.0);
        let value = int + frac1 / 60.0;
        return Some(if !is_neg {value} else {-value});
    }

    None
}

#[test]
fn test_sexagesimal_to_value() {
    assert!(sexagesimal_to_value("").is_none());
    assert!(sexagesimal_to_value("1:00").unwrap() == 1.0);
    assert!(sexagesimal_to_value("-1:00").unwrap() == -1.0);
    assert!(sexagesimal_to_value("10:30").unwrap() == 10.5);
    assert!(sexagesimal_to_value("-10:30").unwrap() == -10.5);
    assert!(sexagesimal_to_value("10:30.3").unwrap() == 10.505);
    assert!(sexagesimal_to_value("-10:30.3").unwrap() == -10.505);
    assert!(sexagesimal_to_value("10:30:00").unwrap() == 10.5);
    assert!(sexagesimal_to_value("10:30:30").unwrap() == 10.508333333333333);
    // TODO: more tests
}
