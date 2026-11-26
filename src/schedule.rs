//! ¿Cuándo toca la próxima copia? Lo decide la app con la hora local.
//!
//! La regla: toca si la última copia correcta es anterior a la última
//! «franja» programada que ya pasó. Así, si el equipo estaba apagado a la
//! hora prevista, la copia se hace en cuanto se enciende.

use chrono::{DateTime, Datelike, Duration, Local, NaiveDate, NaiveTime, TimeZone, Weekday};

use crate::api::Schedule;

/// Tras un fallo, cuánto se espera antes de reintentar la misma franja.
pub const RETRY_AFTER_MINUTES: i64 = 30;

pub fn parse_time(value: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(value.trim(), "%H:%M").ok()
}

fn at(date: NaiveDate, time: NaiveTime) -> Option<DateTime<Local>> {
    Local.from_local_datetime(&date.and_time(time)).earliest()
}

fn weekday_from_iso(n: u8) -> Weekday {
    match n {
        1 => Weekday::Mon,
        2 => Weekday::Tue,
        3 => Weekday::Wed,
        4 => Weekday::Thu,
        5 => Weekday::Fri,
        6 => Weekday::Sat,
        _ => Weekday::Sun,
    }
}

fn month_date(year: i32, month: u32, day: u8) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, month, u32::from(day.clamp(1, 28)))
}

fn prev_month(year: i32, month: u32) -> (i32, u32) {
    if month == 1 {
        (year - 1, 12)
    } else {
        (year, month - 1)
    }
}

fn next_month(year: i32, month: u32) -> (i32, u32) {
    if month == 12 {
        (year + 1, 1)
    } else {
        (year, month + 1)
    }
}

/// Última franja programada que ya ha llegado (<= now).
pub fn previous_slot(s: &Schedule, now: DateTime<Local>) -> Option<DateTime<Local>> {
    if !s.enabled {
        return None;
    }
    let time = parse_time(&s.time)?;
    let today = now.date_naive();
    match s.frequency.as_str() {
        "weekly" => {
            let target = weekday_from_iso(s.weekday.unwrap_or(1));
            (0..8).find_map(|back| {
                let date = today - Duration::days(back);
                if date.weekday() != target {
                    return None;
                }
                at(date, time).filter(|t| *t <= now)
            })
        }
        "monthly" => {
            let day = s.month_day.unwrap_or(1);
            let this = month_date(today.year(), today.month(), day).and_then(|d| at(d, time));
            match this {
                Some(t) if t <= now => Some(t),
                _ => {
                    let (y, m) = prev_month(today.year(), today.month());
                    month_date(y, m, day).and_then(|d| at(d, time))
                }
            }
        }
        _ => match at(today, time) {
            Some(t) if t <= now => Some(t),
            _ => at(today - Duration::days(1), time),
        },
    }
}

/// Próxima franja estrictamente futura.
pub fn next_slot(s: &Schedule, now: DateTime<Local>) -> Option<DateTime<Local>> {
    if !s.enabled {
        return None;
    }
    let time = parse_time(&s.time)?;
    let today = now.date_naive();
    match s.frequency.as_str() {
        "weekly" => {
            let target = weekday_from_iso(s.weekday.unwrap_or(1));
            (0..8).find_map(|ahead| {
                let date = today + Duration::days(ahead);
                if date.weekday() != target {
                    return None;
                }
                at(date, time).filter(|t| *t > now)
            })
        }
        "monthly" => {
            let day = s.month_day.unwrap_or(1);
            match month_date(today.year(), today.month(), day).and_then(|d| at(d, time)) {
                Some(t) if t > now => Some(t),
                _ => {
                    let (y, m) = next_month(today.year(), today.month());
                    month_date(y, m, day).and_then(|d| at(d, time))
                }
            }
        }
        _ => match at(today, time) {
            Some(t) if t > now => Some(t),
            _ => at(today + Duration::days(1), time),
        },
    }
}

/// ¿Hay que lanzar ya la copia programada?
pub fn is_due(
    s: &Schedule,
    last_success: Option<DateTime<Local>>,
    last_attempt: Option<DateTime<Local>>,
    now: DateTime<Local>,
) -> bool {
    let Some(slot) = previous_slot(s, now) else {
        return false;
    };
    if last_success.is_some_and(|t| t >= slot) {
        return false;
    }
    match last_attempt {
        Some(t) if t >= slot => now - t >= Duration::minutes(RETRY_AFTER_MINUTES),
        _ => true,
    }
}

/// «Cada día a las 03:00», etc.
pub fn describe(s: &Schedule) -> String {
    if !s.enabled {
        return "Desactivada".into();
    }
    match s.frequency.as_str() {
        "weekly" => format!(
            "Cada {} a las {}",
            weekday_name(s.weekday.unwrap_or(1)).to_lowercase(),
            s.time
        ),
        "monthly" => format!(
            "El día {} de cada mes a las {}",
            s.month_day.unwrap_or(1),
            s.time
        ),
        _ => format!("Cada día a las {}", s.time),
    }
}

pub fn weekday_name(n: u8) -> &'static str {
    [
        "Lunes",
        "Martes",
        "Miércoles",
        "Jueves",
        "Viernes",
        "Sábado",
        "Domingo",
    ][usize::from(n.clamp(1, 7) - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, m, d, h, min, 0).single().unwrap()
    }

    fn sched(freq: &str, time: &str, weekday: Option<u8>, month_day: Option<u8>) -> Schedule {
        Schedule {
            enabled: true,
            frequency: freq.into(),
            time: time.into(),
            weekday,
            month_day,
            ..Default::default()
        }
    }

    #[test]
    fn diaria() {
        let s = sched("daily", "03:00", None, None);
        let now = local(2026, 11, 26, 10, 0);
        assert_eq!(previous_slot(&s, now), Some(local(2026, 11, 26, 3, 0)));
        assert_eq!(next_slot(&s, now), Some(local(2026, 11, 27, 3, 0)));
        let early = local(2026, 11, 26, 2, 0);
        assert_eq!(previous_slot(&s, early), Some(local(2026, 11, 25, 3, 0)));
        assert_eq!(next_slot(&s, early), Some(local(2026, 11, 26, 3, 0)));
    }

    #[test]
    fn semanal() {
        // 26/11/2026 es jueves
        let s = sched("weekly", "22:30", Some(5), None); // viernes
        let now = local(2026, 11, 26, 12, 0);
        assert_eq!(previous_slot(&s, now), Some(local(2026, 11, 20, 22, 30)));
        assert_eq!(next_slot(&s, now), Some(local(2026, 11, 27, 22, 30)));
        let s = sched("weekly", "08:00", Some(4), None); // jueves, ya pasó
        assert_eq!(previous_slot(&s, now), Some(local(2026, 11, 26, 8, 0)));
        assert_eq!(next_slot(&s, now), Some(local(2026, 12, 3, 8, 0)));
    }

    #[test]
    fn mensual() {
        let s = sched("monthly", "04:00", None, Some(1));
        let now = local(2026, 1, 15, 9, 0);
        assert_eq!(previous_slot(&s, now), Some(local(2026, 1, 1, 4, 0)));
        assert_eq!(next_slot(&s, now), Some(local(2026, 2, 1, 4, 0)));
        let s = sched("monthly", "04:00", None, Some(28));
        assert_eq!(previous_slot(&s, now), Some(local(2025, 12, 28, 4, 0)));
        let dec = local(2026, 12, 29, 9, 0);
        assert_eq!(next_slot(&s, dec), Some(local(2027, 1, 28, 4, 0)));
    }

    #[test]
    fn cuando_toca() {
        let s = sched("daily", "03:00", None, None);
        let now = local(2026, 11, 26, 10, 0);
        // nunca se hizo: toca
        assert!(is_due(&s, None, None, now));
        // hecha hoy tras las 03:00: no toca
        assert!(!is_due(&s, Some(local(2026, 11, 26, 3, 5)), None, now));
        // hecha ayer: toca (el equipo estaba apagado a las 03:00)
        assert!(is_due(&s, Some(local(2026, 11, 25, 3, 5)), None, now));
        // falló hace 10 min: espera
        assert!(!is_due(&s, None, Some(local(2026, 11, 26, 9, 50)), now));
        // falló hace 40 min: reintenta
        assert!(is_due(&s, None, Some(local(2026, 11, 26, 9, 20)), now));
        // desactivada: nunca
        let off = Schedule {
            enabled: false,
            ..s.clone()
        };
        assert!(!is_due(&off, None, None, now));
    }

    #[test]
    fn descripcion() {
        assert_eq!(
            describe(&sched("weekly", "22:30", Some(3), None)),
            "Cada miércoles a las 22:30"
        );
        assert_eq!(
            describe(&sched("daily", "03:00", None, None)),
            "Cada día a las 03:00"
        );
    }
}
