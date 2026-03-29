use std::collections::BTreeMap;
use std::rc::Rc;

use crate::value::{VmError, VmValue};
use crate::vm::Vm;

pub(crate) fn register_datetime_builtins(vm: &mut Vm) {
    vm.register_builtin("date_now", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let (y, m, d, hour, minute, second, dow) = vm_civil_from_timestamp(total_secs);
        let mut result = BTreeMap::new();
        result.insert("year".to_string(), VmValue::Int(y));
        result.insert("month".to_string(), VmValue::Int(m));
        result.insert("day".to_string(), VmValue::Int(d));
        result.insert("hour".to_string(), VmValue::Int(hour));
        result.insert("minute".to_string(), VmValue::Int(minute));
        result.insert("second".to_string(), VmValue::Int(second));
        result.insert("weekday".to_string(), VmValue::Int(dow));
        result.insert("timestamp".to_string(), VmValue::Float(now.as_secs_f64()));
        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("date_format", |args, _out| {
        let ts = match args.first() {
            Some(VmValue::Float(f)) => *f,
            Some(VmValue::Int(n)) => *n as f64,
            Some(VmValue::Dict(map)) => map
                .get("timestamp")
                .and_then(|v| match v {
                    VmValue::Float(f) => Some(*f),
                    VmValue::Int(n) => Some(*n as f64),
                    _ => None,
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        let fmt = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());

        let (y, m, d, hour, minute, second, _dow) = vm_civil_from_timestamp(ts as u64);

        let result = fmt
            .replace("%Y", &format!("{y:04}"))
            .replace("%m", &format!("{m:02}"))
            .replace("%d", &format!("{d:02}"))
            .replace("%H", &format!("{hour:02}"))
            .replace("%M", &format!("{minute:02}"))
            .replace("%S", &format!("{second:02}"));

        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    vm.register_builtin("date_parse", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let parts: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).collect();
        let parts: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        if parts.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Cannot parse date: {s}"
            )))));
        }
        let (y, m, d) = (parts[0], parts[1], parts[2]);
        let hour = parts.get(3).copied().unwrap_or(0);
        let minute = parts.get(4).copied().unwrap_or(0);
        let second = parts.get(5).copied().unwrap_or(0);

        let (y_adj, m_adj) = if m <= 2 {
            (y - 1, (m + 9) as u64)
        } else {
            (y, (m - 3) as u64)
        };
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = (y_adj - era * 400) as u64;
        let doy = (153 * m_adj + 2) / 5 + d as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe as i64 - 719468;
        let ts = days * 86400 + hour * 3600 + minute * 60 + second;
        Ok(VmValue::Float(ts as f64))
    });
}

/// Civil date from unix timestamp (Howard Hinnant's algorithm).
pub(crate) fn vm_civil_from_timestamp(total_secs: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = (time_of_day / 3600) as i64;
    let minute = ((time_of_day % 3600) / 60) as i64;
    let second = (time_of_day % 60) as i64;

    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    let y = if m <= 2 { y + 1 } else { y };
    let dow = ((days + 4) % 7) as i64;

    (y, m, d, hour, minute, second, dow)
}
