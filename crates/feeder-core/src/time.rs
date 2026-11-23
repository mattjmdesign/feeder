use std::time::{SystemTime, UNIX_EPOCH};

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let s = ((now_ms - then_ms).max(0) / 1000) as u64;
    match s {
        0..60 => "just now".to_string(),
        60..3_600 => format!("{}m", s / 60),
        3_600..86_400 => format!("{}h", s / 3_600),
        86_400..604_800 => format!("{}d", s / 86_400),
        604_800..2_592_000 => format!("{}w", s / 604_800),
        2_592_000..31_536_000 => format!("{}mo", s / 2_592_000),
        _ => format!("{}y", s / 31_536_000),
    }
}

pub fn ago(then_ms: i64, now_ms: i64) -> String {
    let t = relative_time(then_ms, now_ms);
    if t == "just now" { t } else { format!("{t} ago") }
}

fn local_ymd(ms: i64) -> (i32, u8, u8, u8) {
    let secs = ms.div_euclid(1000) as libc::time_t;
    #[cfg(unix)]
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if !libc::localtime_r(&secs, &mut tm).is_null() {
            return (tm.tm_year + 1900, (tm.tm_mon + 1).clamp(1, 12) as u8, tm.tm_mday.clamp(1, 31) as u8, tm.tm_wday.clamp(0, 6) as u8);
        }
    }
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    let wday = (days + 4).rem_euclid(7) as u8;
    (y, m, d, wday)
}

pub fn day_key(ms: i64) -> i64 {
    let (y, m, d, _) = local_ymd(ms);
    days_from_civil(y, m, d)
}

pub fn day_label(then_ms: i64, now_ms: i64) -> String {
    let (ty, tm, td, tw) = local_ymd(then_ms);
    let (ny, nm, nd, _) = local_ymd(now_ms);
    let diff = days_from_civil(ny, nm, nd) - days_from_civil(ty, tm, td);
    match diff {
        0 => "Today".to_string(),
        1 => "Yesterday".to_string(),
        _ => {
            const WDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
            const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
            let mon = MONTHS[(tm as usize).saturating_sub(1).min(11)];
            if ty == ny { format!("{}, {mon} {td}", WDAYS[(tw as usize).min(6)]) } else { format!("{mon} {td}, {ty}") }
        }
    }
}

fn days_from_civil(y: i32, m: u8, d: u8) -> i64 {
    let y = i64::from(y) - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let m = i64::from(m);
    let doy = (153 * (m + if m > 2 { -3 } else { 9 }) + 2) / 5 + i64::from(d) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i32, u8, u8) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = mp + if mp < 10 { 3 } else { -9 };
    ((y + i64::from(m <= 2)) as i32, m as u8, d as u8)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_read_short() {
        let now = 10_000_000_000;
        assert_eq!(relative_time(now - 5_000, now), "just now");
        assert_eq!(relative_time(now - 2 * 3_600_000, now), "2h");
        assert_eq!(relative_time(now - 86_400_000, now), "1d");
        assert_eq!(relative_time(now + 9_000, now), "just now");
    }

    #[test]
    fn day_labels_group_today_yesterday_and_earlier() {
        let now = now_ms();
        assert_eq!(day_label(now, now), "Today");
        assert_eq!(day_label(now - 86_400_000, now), "Yesterday");
        assert_eq!(day_key(now) - day_key(now - 86_400_000), 1);
        let earlier = day_label(now - 5 * 86_400_000, now);
        assert!(earlier.contains(','));
    }
}
