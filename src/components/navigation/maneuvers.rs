//! Walking instructions from a route's steps. MOTIS' foot router names each step's street but
//! gives no turns (`relativeDirection` is only ever `CONTINUE` or `STAIRS`), so turns come from
//! the geometry: the change of heading where one step meets the next, each heading taken over a
//! few metres so that tiny kinks and GPS-scale wiggles don't count. Consecutive steps that run
//! straight on along the same street make a single instruction.

use bytes::Bytes;
use freya::icons::lucide;

use super::geo::{bearing, cumulative, distance_m, point_at, turn_angle};

/// Headings either side of a step join are taken over this distance.
const HEADING_SPAN_M: f64 = 15.0;
/// Two turns closer than this make one (a short dog-leg through a crossing).
const MERGE_TURNS_M: f64 = 12.0;

/// One step of a route as the router gives it.
#[derive(Clone, Debug, PartialEq)]
pub struct RawStep {
    /// `relativeDirection`: `CONTINUE`, `STAIRS`, …
    pub direction: String,
    pub street: String,
    /// `(lon, lat)` path.
    pub points: Vec<(f64, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManeuverKind {
    Depart,
    /// Straight on, onto a differently named street.
    Continue,
    SlightLeft,
    Left,
    SharpLeft,
    SlightRight,
    Right,
    SharpRight,
    UTurn,
    /// Round a roundabout, leaving by `Maneuver::exit`.
    Roundabout,
    Stairs,
    Arrive,
}

impl ManeuverKind {
    fn is_turn(self) -> bool {
        matches!(
            self,
            Self::SlightLeft
                | Self::Left
                | Self::SharpLeft
                | Self::SlightRight
                | Self::Right
                | Self::SharpRight
                | Self::UTurn
        )
    }
}

/// A turn of `angle` degrees (positive right), or `None` when it's straight on.
pub fn classify_turn(angle: f64) -> Option<ManeuverKind> {
    let size = angle.abs();
    let right = angle > 0.0;
    let kind = if size < 30.0 {
        return None;
    } else if size < 60.0 {
        if right {
            ManeuverKind::SlightRight
        } else {
            ManeuverKind::SlightLeft
        }
    } else if size < 135.0 {
        if right {
            ManeuverKind::Right
        } else {
            ManeuverKind::Left
        }
    } else if size < 165.0 {
        if right {
            ManeuverKind::SharpRight
        } else {
            ManeuverKind::SharpLeft
        }
    } else {
        ManeuverKind::UTurn
    };
    Some(kind)
}

/// One instruction.
#[derive(Clone, Debug, PartialEq)]
pub struct Maneuver {
    pub kind: ManeuverKind,
    /// The street it leads onto, when named.
    pub street: Option<String>,
    /// Index in the route's points where it happens.
    pub at: usize,
    /// Metres from here to the next instruction.
    pub distance_m: f64,
    /// For `Depart`: the heading to set off in.
    pub heading: f64,
    /// For `Roundabout`: which exit (1 = the first).
    pub exit: Option<u32>,
}

/// A step with its instruction already known, as IGN's router gives them: the instruction
/// applies where the step starts, its street and path are what follows.
#[derive(Clone, Debug, PartialEq)]
pub struct GivenStep {
    pub kind: ManeuverKind,
    pub street: Option<String>,
    pub exit: Option<u32>,
    /// `(lon, lat)` path.
    pub points: Vec<(f64, f64)>,
}

/// An OSRM-style instruction (`type`, `modifier`), as IGN's router gives it. Valhalla also
/// says "continue" or "fork" with a side: the side decides.
pub fn kind_from_instruction(kind: &str, modifier: Option<&str>) -> ManeuverKind {
    match kind {
        "depart" => return ManeuverKind::Depart,
        "arrive" => return ManeuverKind::Arrive,
        "roundabout" | "rotary" | "roundabout turn" => return ManeuverKind::Roundabout,
        _ => {}
    }
    match modifier {
        Some("slight left") => ManeuverKind::SlightLeft,
        Some("left") => ManeuverKind::Left,
        Some("sharp left") => ManeuverKind::SharpLeft,
        Some("slight right") => ManeuverKind::SlightRight,
        Some("right") => ManeuverKind::Right,
        Some("sharp right") => ManeuverKind::SharpRight,
        Some("uturn") => ManeuverKind::UTurn,
        _ => ManeuverKind::Continue,
    }
}

/// The route's path and instructions from steps whose instructions are known (see
/// [`GivenStep`]). Straight-on steps along the street already walked add nothing.
pub fn assemble(steps: &[GivenStep]) -> (Vec<(f64, f64)>, Vec<Maneuver>) {
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut maneuvers: Vec<Maneuver> = Vec::new();
    let mut street: Option<String> = None;
    for step in steps {
        let mut step_points = step.points.iter().copied();
        let start = match (points.last(), step.points.first()) {
            (Some(last), Some(first)) if distance_m(*last, *first) < 0.5 => {
                step_points.next();
                points.len() - 1
            }
            _ => points.len(),
        };
        points.extend(step_points);
        let same_street = step.street.is_some() && step.street == street;
        let redundant = match step.kind {
            ManeuverKind::Depart => !maneuvers.is_empty(),
            ManeuverKind::Arrive => false,
            ManeuverKind::Continue => same_street || step.street.is_none(),
            _ => false,
        };
        if step.street.is_some() {
            street = step.street.clone();
        }
        if redundant || step.kind == ManeuverKind::Arrive {
            continue;
        }
        maneuvers.push(Maneuver {
            kind: step.kind,
            // A bend along the street already walked isn't "onto" it.
            street: if same_street {
                None
            } else {
                step.street.clone()
            },
            at: start.min(points.len().saturating_sub(1)),
            distance_m: 0.0,
            heading: 0.0,
            exit: step.exit,
        });
    }
    if points.len() < 2 {
        return (points, Vec::new());
    }
    let along = cumulative(&points);
    match maneuvers.first_mut() {
        Some(first) if first.kind == ManeuverKind::Depart => first.at = 0,
        _ => maneuvers.insert(0, maneuver(ManeuverKind::Depart, None, 0)),
    }
    maneuvers[0].heading = bearing(points[0], point_at(&points, &along, HEADING_SPAN_M));
    maneuvers.push(maneuver(ManeuverKind::Arrive, None, points.len() - 1));
    for index in 0..maneuvers.len() - 1 {
        maneuvers[index].distance_m = along[maneuvers[index + 1].at] - along[maneuvers[index].at];
    }
    (points, maneuvers)
}

/// The route's whole path and its instructions, from `Depart` to `Arrive`.
pub fn build(steps: &[RawStep]) -> (Vec<(f64, f64)>, Vec<Maneuver>) {
    // One path, remembering where each step starts in it.
    let mut points: Vec<(f64, f64)> = Vec::new();
    let mut starts = Vec::with_capacity(steps.len());
    for step in steps {
        let mut step_points = step.points.iter().copied();
        // Steps usually share their joining point: keep it once, as this step's start.
        let start = match (points.last(), step.points.first()) {
            (Some(last), Some(first)) if distance_m(*last, *first) < 0.5 => {
                step_points.next();
                points.len() - 1
            }
            _ => points.len(),
        };
        starts.push(start);
        points.extend(step_points);
    }
    if points.len() < 2 {
        return (points, Vec::new());
    }
    let along = cumulative(&points);
    let heading_at = |at: f64, ahead: bool| {
        let (a, b) = if ahead {
            (at, at + HEADING_SPAN_M)
        } else {
            (at - HEADING_SPAN_M, at)
        };
        bearing(point_at(&points, &along, a), point_at(&points, &along, b))
    };

    // The street set off along: the first step's, else one named within the first few metres
    // (routes often start on an unnamed footway joining it).
    let depart_street = steps
        .iter()
        .zip(&starts)
        .take_while(|(_, start)| along[(**start).min(along.len() - 1)] < 30.0)
        .find(|(step, _)| !step.street.is_empty())
        .map(|(step, _)| step.street.clone());
    let mut maneuvers = vec![Maneuver {
        kind: ManeuverKind::Depart,
        street: depart_street,
        at: 0,
        distance_m: 0.0,
        heading: heading_at(0.0, true),
        exit: None,
    }];
    let mut street = steps
        .first()
        .map(|step| step.street.clone())
        .unwrap_or_default();

    for (index, step) in steps.iter().enumerate().skip(1) {
        let at = starts[index];
        let named = (!step.street.is_empty()).then(|| step.street.clone());
        if step.direction == "STAIRS" {
            if maneuvers
                .last()
                .is_none_or(|last| last.kind != ManeuverKind::Stairs)
            {
                maneuvers.push(maneuver(ManeuverKind::Stairs, named.clone(), at));
            }
        } else if let Some(kind) = classify_turn(turn_angle(
            heading_at(along[at], false),
            heading_at(along[at], true),
        )) {
            maneuvers.push(maneuver(kind, named.clone(), at));
        } else if let Some(name) = &named
            && *name != street
        {
            maneuvers.push(maneuver(ManeuverKind::Continue, named.clone(), at));
        }
        if let Some(name) = named {
            street = name;
        }
    }

    // A turn just after another is one manoeuvre through a crossing: keep the overall turn.
    let mut merged: Vec<Maneuver> = Vec::with_capacity(maneuvers.len());
    for next in maneuvers {
        if let Some(previous) = merged.last_mut()
            && previous.kind.is_turn()
            && next.kind.is_turn()
            && along[next.at] - along[previous.at] < MERGE_TURNS_M
        {
            let overall = turn_angle(
                heading_at(along[previous.at], false),
                heading_at(along[next.at], true),
            );
            match classify_turn(overall) {
                Some(kind) => {
                    previous.kind = kind;
                    previous.street = next.street.or(previous.street.take());
                }
                None => {
                    merged.pop();
                }
            }
            continue;
        }
        merged.push(next);
    }
    merged.push(maneuver(ManeuverKind::Arrive, None, points.len() - 1));

    for index in 0..merged.len() - 1 {
        merged[index].distance_m = along[merged[index + 1].at] - along[merged[index].at];
    }
    (points, merged)
}

fn maneuver(kind: ManeuverKind, street: Option<String>, at: usize) -> Maneuver {
    Maneuver {
        kind,
        street,
        at,
        distance_m: 0.0,
        heading: 0.0,
        exit: None,
    }
}

/// The instruction, as shown: "Turn left onto Rue de Rivoli". `destination` names `Arrive`.
pub fn instruction_text(maneuver: &Maneuver, destination: &str) -> String {
    let onto = |verb: &str| match &maneuver.street {
        Some(street) => format!("{verb} onto {street}"),
        None => verb.to_string(),
    };
    match maneuver.kind {
        ManeuverKind::Depart => {
            let direction = compass_word(maneuver.heading);
            match &maneuver.street {
                Some(street) => format!("Head {direction} on {street}"),
                None => format!("Head {direction}"),
            }
        }
        ManeuverKind::Continue => onto("Continue"),
        ManeuverKind::SlightLeft => onto("Turn slightly left"),
        ManeuverKind::Left => onto("Turn left"),
        ManeuverKind::SharpLeft => onto("Turn sharp left"),
        ManeuverKind::SlightRight => onto("Turn slightly right"),
        ManeuverKind::Right => onto("Turn right"),
        ManeuverKind::SharpRight => onto("Turn sharp right"),
        ManeuverKind::UTurn => "Make a U-turn".to_string(),
        ManeuverKind::Roundabout => match maneuver.exit {
            Some(exit) => onto(&format!(
                "At the roundabout, take the {} exit",
                ordinal(exit)
            )),
            None => onto("Go round the roundabout"),
        },
        ManeuverKind::Stairs => "Take the stairs".to_string(),
        ManeuverKind::Arrive if destination.is_empty() => "Arrive".to_string(),
        ManeuverKind::Arrive => format!("Arrive at {destination}"),
    }
}

fn ordinal(n: u32) -> String {
    let suffix = match (n % 10, n % 100) {
        (_, 11..=13) => "th",
        (1, _) => "st",
        (2, _) => "nd",
        (3, _) => "rd",
        _ => "th",
    };
    format!("{n}{suffix}")
}

fn compass_word(heading: f64) -> &'static str {
    const WORDS: [&str; 8] = [
        "north",
        "northeast",
        "east",
        "southeast",
        "south",
        "southwest",
        "west",
        "northwest",
    ];
    WORDS[((heading.rem_euclid(360.0) + 22.5) / 45.0) as usize % 8]
}

pub fn maneuver_icon(kind: ManeuverKind) -> Bytes {
    match kind {
        ManeuverKind::Depart => lucide::footprints(),
        ManeuverKind::Continue => lucide::arrow_up(),
        ManeuverKind::SlightLeft => lucide::arrow_up_left(),
        ManeuverKind::Left | ManeuverKind::SharpLeft => lucide::corner_up_left(),
        ManeuverKind::SlightRight => lucide::arrow_up_right(),
        ManeuverKind::Right | ManeuverKind::SharpRight => lucide::corner_up_right(),
        ManeuverKind::UTurn => lucide::undo_2(),
        ManeuverKind::Roundabout => lucide::rotate_cw(),
        ManeuverKind::Stairs => lucide::footprints(),
        ManeuverKind::Arrive => lucide::flag(),
    }
}

/// `"120 m"`, `"1.4 km"`, rounded the way walking distances are read.
pub fn distance_text(metres: f64) -> String {
    if metres < 50.0 {
        format!("{} m", (metres / 5.0).round() as i64 * 5)
    } else if metres < 1000.0 {
        format!("{} m", (metres / 10.0).round() as i64 * 10)
    } else {
        format!("{:.1} km", metres / 1000.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// About 1 m in degrees of latitude.
    const M: f64 = 1.0 / 111_320.0;

    /// A step from `points` given as metres (east, north) from a Paris origin.
    fn step(direction: &str, street: &str, metres: &[(f64, f64)]) -> RawStep {
        let lat0 = 48.85f64;
        let east = M / lat0.to_radians().cos();
        RawStep {
            direction: direction.into(),
            street: street.into(),
            points: metres
                .iter()
                .map(|(x, y)| (2.35 + x * east, lat0 + y * M))
                .collect(),
        }
    }

    #[test]
    fn turns_from_geometry() {
        assert_eq!(classify_turn(10.0), None);
        assert_eq!(classify_turn(-45.0), Some(ManeuverKind::SlightLeft));
        assert_eq!(classify_turn(90.0), Some(ManeuverKind::Right));
        assert_eq!(classify_turn(-150.0), Some(ManeuverKind::SharpLeft));
        assert_eq!(classify_turn(178.0), Some(ManeuverKind::UTurn));
    }

    #[test]
    fn l_shaped_walk_turns_left() {
        // North 100 m along Rue A, then west 80 m along Rue B.
        let steps = [
            step("CONTINUE", "Rue A", &[(0.0, 0.0), (0.0, 100.0)]),
            step("CONTINUE", "Rue B", &[(0.0, 100.0), (-80.0, 100.0)]),
        ];
        let (points, maneuvers) = build(&steps);
        assert_eq!(points.len(), 3);
        let kinds: Vec<_> = maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                ManeuverKind::Depart,
                ManeuverKind::Left,
                ManeuverKind::Arrive
            ]
        );
        assert_eq!(maneuvers[1].street.as_deref(), Some("Rue B"));
        assert!((maneuvers[0].distance_m - 100.0).abs() < 1.0);
        assert!((maneuvers[1].distance_m - 80.0).abs() < 1.0);
        assert_eq!(instruction_text(&maneuvers[0], ""), "Head north on Rue A");
        assert_eq!(instruction_text(&maneuvers[1], ""), "Turn left onto Rue B");
        assert_eq!(
            instruction_text(&maneuvers[2], "Louvre"),
            "Arrive at Louvre"
        );
    }

    #[test]
    fn straight_same_street_steps_merge_and_renames_continue() {
        let steps = [
            step("CONTINUE", "Rue A", &[(0.0, 0.0), (0.0, 50.0)]),
            step("CONTINUE", "Rue A", &[(0.0, 50.0), (2.0, 100.0)]),
            step("CONTINUE", "", &[(2.0, 100.0), (2.0, 110.0)]),
            step("CONTINUE", "Rue C", &[(2.0, 110.0), (2.0, 200.0)]),
        ];
        let kinds: Vec<_> = build(&steps).1.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                ManeuverKind::Depart,
                ManeuverKind::Continue,
                ManeuverKind::Arrive
            ]
        );
    }

    #[test]
    fn dog_leg_through_a_crossing_is_one_turn_or_none() {
        // Right 6 m then left: a jog across a road, straight on overall.
        let steps = [
            step("CONTINUE", "Rue A", &[(0.0, 0.0), (0.0, 100.0)]),
            step("CONTINUE", "", &[(0.0, 100.0), (6.0, 100.0)]),
            step("CONTINUE", "Rue A", &[(6.0, 100.0), (6.0, 200.0)]),
        ];
        let kinds: Vec<_> = build(&steps).1.iter().map(|m| m.kind).collect();
        assert_eq!(kinds, [ManeuverKind::Depart, ManeuverKind::Arrive]);
    }

    #[test]
    fn stairs_are_one_instruction() {
        let steps = [
            step("CONTINUE", "Rue A", &[(0.0, 0.0), (0.0, 50.0)]),
            step("STAIRS", "", &[(0.0, 50.0), (0.0, 55.0)]),
            step("STAIRS", "", &[(0.0, 55.0), (0.0, 60.0)]),
            step("CONTINUE", "Rue A", &[(0.0, 60.0), (0.0, 120.0)]),
        ];
        let maneuvers = build(&steps).1;
        let kinds: Vec<_> = maneuvers.iter().map(|m| m.kind).collect();
        assert_eq!(
            kinds,
            [
                ManeuverKind::Depart,
                ManeuverKind::Stairs,
                ManeuverKind::Arrive
            ]
        );
        assert_eq!(instruction_text(&maneuvers[1], ""), "Take the stairs");
    }

    #[test]
    fn distances_read_naturally() {
        assert_eq!(distance_text(12.0), "10 m");
        assert_eq!(distance_text(123.0), "120 m");
        assert_eq!(distance_text(1449.0), "1.4 km");
    }
}
