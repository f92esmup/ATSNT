//! The decision stage of an agent: the rule table, its generated mirror, and the explicit vote
//! resolution (strategy spec §8.3–8.4).
//!
//! Conditions are data ([`Cond`]), so the mirror is a mechanical transformation and every rule is
//! evaluated: there is no `if / else if` priority chain whose source order would silently decide
//! between contradictory rules (correction C3).

use std::fmt;

use domain::{FeedBar, IndicatorSnapshot};
use serde::{Deserialize, Serialize};

use super::agent::{
    macro_states, micro_patterns, Direction, MacroSet, MacroState, MicroSet, Pattern,
};
use super::genome::Genome;

/// A rule condition, built from a small combinator type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cond {
    /// Every sub-condition holds.
    All(&'static [Cond]),
    /// At least one sub-condition holds.
    Any(&'static [Cond]),
    /// The sub-condition does not hold.
    Not(&'static Cond),
    /// A macro state holds.
    Macro(MacroState),
    /// A micro pattern with that direction was found.
    Micro(Pattern, Direction),
}

impl Cond {
    /// Evaluates the condition. With `mirrored`, every leaf is read through the mirror
    /// transformation of §8.3 (`Bullish ↔ Bearish`, `Overbought ↔ Oversold`, `Up ↔ Down`).
    pub fn eval(&self, mirrored: bool, macros: &MacroSet, micros: &MicroSet) -> bool {
        match self {
            Cond::All(parts) => parts.iter().all(|c| c.eval(mirrored, macros, micros)),
            Cond::Any(parts) => parts.iter().any(|c| c.eval(mirrored, macros, micros)),
            Cond::Not(inner) => !inner.eval(mirrored, macros, micros),
            Cond::Macro(state) => macros.contains(if mirrored { state.mirror() } else { *state }),
            Cond::Micro(pattern, direction) => micros.contains(
                *pattern,
                if mirrored {
                    direction.mirror()
                } else {
                    *direction
                },
            ),
        }
    }

    fn render(&self, mirrored: bool, nested: bool) -> String {
        match self {
            Cond::All(parts) => join(parts, " ∧ ", mirrored, nested),
            Cond::Any(parts) => join(parts, " ∨ ", mirrored, nested),
            Cond::Not(inner) => format!("¬{}", inner.render(mirrored, true)),
            Cond::Macro(state) => {
                let state = if mirrored { state.mirror() } else { *state };
                format!("{state:?}")
            }
            Cond::Micro(pattern, direction) => {
                let direction = if mirrored {
                    direction.mirror()
                } else {
                    *direction
                };
                let name = match pattern {
                    Pattern::PinBar => "PinBar",
                    Pattern::Engulfing => "Engulfing",
                    Pattern::InsideBar => "InsideBar",
                    Pattern::BreakOfStructure => "BoS",
                };
                let arrow = match direction {
                    Direction::Up => "↑",
                    Direction::Down => "↓",
                    Direction::Neutral => "",
                };
                format!("{name}{arrow}")
            }
        }
    }
}

fn join(parts: &[Cond], separator: &str, mirrored: bool, nested: bool) -> String {
    let body = parts
        .iter()
        .map(|c| c.render(mirrored, true))
        .collect::<Vec<_>>()
        .join(separator);
    if nested && parts.len() > 1 {
        format!("({body})")
    } else {
        body
    }
}

/// What a rule asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Action {
    /// Vote +1 on the long axis.
    LongEntry,
    /// Vote -1 on the long axis.
    LongExit,
    /// Vote +1 on the short axis.
    ShortEntry,
    /// Vote -1 on the short axis.
    ShortExit,
}

impl Action {
    /// `LongEntry ↔ ShortEntry`, `LongExit ↔ ShortExit`.
    pub fn mirror(self) -> Self {
        match self {
            Action::LongEntry => Action::ShortEntry,
            Action::LongExit => Action::ShortExit,
            Action::ShortEntry => Action::LongEntry,
            Action::ShortExit => Action::LongExit,
        }
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// The set of actions fired by one evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct ActionSet(u8);

impl ActionSet {
    /// The set of the given actions.
    pub fn of(actions: &[Action]) -> Self {
        let mut set = Self::default();
        for action in actions {
            set.insert(*action);
        }
        set
    }

    /// Adds an action.
    pub fn insert(&mut self, action: Action) {
        self.0 |= action.bit();
    }

    /// Whether the action fired.
    pub fn contains(&self, action: Action) -> bool {
        self.0 & action.bit() != 0
    }
}

/// The name of an original (long-axis) rule, as in spec §8.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RuleName {
    /// `decision-comprar-continuacion-alcista`.
    E1,
    /// `decision-comprar-ruptura-estructural`.
    E2,
    /// `decision-comprar-rebote-sobrevendido`.
    E3,
    /// `decision-comprar-acumulacion`.
    E4,
    /// `decision-comprar-fuerza-bruta`.
    E5,
    /// `decision-reducir-agotamiento-alcista`.
    X1,
    /// `decision-reducir-falta-liquidez`.
    X2,
    /// `decision-reducir-cambio-tendencia`.
    X3,
    /// `decision-reducir-freno-volatilidad`.
    X4,
}

/// Number of rules with their mirrors.
pub const RULE_COUNT: usize = 18;

/// The id of a rule: its original name and whether it is the generated mirror (`′`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RuleId {
    /// The original rule.
    pub name: RuleName,
    /// True for the generated short-axis mirror.
    pub mirrored: bool,
}

impl RuleId {
    /// A dense index in `0..RULE_COUNT`: originals first, then mirrors.
    pub fn index(&self) -> usize {
        self.name as usize + if self.mirrored { RULE_COUNT / 2 } else { 0 }
    }

    /// Every id in index order.
    pub fn all() -> [RuleId; RULE_COUNT] {
        std::array::from_fn(|i| RuleId {
            name: LONG_RULES[i % (RULE_COUNT / 2)].id.name,
            mirrored: i >= RULE_COUNT / 2,
        })
    }
}

impl fmt::Display for RuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}{}", self.name, if self.mirrored { "′" } else { "" })
    }
}

/// One rule of the table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    /// Rule id.
    pub id: RuleId,
    /// Action taken when the condition holds.
    pub action: Action,
    /// The condition, written for the original orientation.
    pub cond: &'static Cond,
}

impl Rule {
    /// Whether the rule fires, reading its condition through the mirror when it is one.
    pub fn fires(&self, macros: &MacroSet, micros: &MicroSet) -> bool {
        self.cond.eval(self.id.mirrored, macros, micros)
    }

    /// The condition in the notation of spec §8.3, e.g. `Bullish ∧ BoS↑`.
    pub fn describe(&self) -> String {
        self.cond.render(self.id.mirrored, false)
    }
}

const fn original(name: RuleName, action: Action, cond: &'static Cond) -> Rule {
    Rule {
        id: RuleId {
            name,
            mirrored: false,
        },
        action,
        cond,
    }
}

use Cond::{All, Any, Macro, Micro, Not};
use Direction::{Down, Neutral, Up};
use MacroState::{Bearish, Bullish, High, Low, Overbought, Oversold, Stretched};
use Pattern::{BreakOfStructure, Engulfing, InsideBar, PinBar};

/// The long-axis rules of `03_decision.clp` (§8.3). The short axis is generated by [`mirror`].
pub const LONG_RULES: &[Rule] = &[
    original(
        RuleName::E1,
        Action::LongEntry,
        &All(&[
            Macro(Bullish),
            Macro(High),
            Any(&[Micro(PinBar, Up), Micro(Engulfing, Up)]),
        ]),
    ),
    original(
        RuleName::E2,
        Action::LongEntry,
        &All(&[Macro(Bullish), Micro(BreakOfStructure, Up)]),
    ),
    original(
        RuleName::E3,
        Action::LongEntry,
        &All(&[
            Macro(Oversold),
            Macro(Stretched),
            Any(&[Micro(PinBar, Up), Micro(Engulfing, Up)]),
        ]),
    ),
    original(
        RuleName::E4,
        Action::LongEntry,
        &All(&[Macro(Bullish), Macro(Low), Micro(InsideBar, Neutral)]),
    ),
    original(
        RuleName::E5,
        Action::LongEntry,
        &All(&[
            Macro(Bullish),
            Macro(High),
            Not(&Macro(Overbought)),
            Any(&[Micro(BreakOfStructure, Up), Micro(Engulfing, Up)]),
        ]),
    ),
    original(
        RuleName::X1,
        Action::LongExit,
        &All(&[
            Macro(Overbought),
            Any(&[Micro(PinBar, Down), Micro(BreakOfStructure, Down)]),
        ]),
    ),
    original(
        RuleName::X2,
        Action::LongExit,
        &All(&[Macro(Stretched), Micro(InsideBar, Neutral)]),
    ),
    original(RuleName::X3, Action::LongExit, &Macro(Bearish)),
    original(
        RuleName::X4,
        Action::LongExit,
        &All(&[Macro(Low), Macro(Overbought)]),
    ),
];

/// Generates the mirror of a rule: the same condition read through the mirror transformation,
/// the mirrored action, and the `′` id. `mirror(mirror(r)) == r`.
pub fn mirror(rule: &Rule) -> Rule {
    Rule {
        id: RuleId {
            name: rule.id.name,
            mirrored: !rule.id.mirrored,
        },
        action: rule.action.mirror(),
        cond: rule.cond,
    }
}

/// The rules an agent evaluates: `LONG_RULES`, plus their mirrors when shorts are allowed.
/// In long-only mode the mirrors are not evaluated at all (§8.4 step 7).
pub fn rule_book(allow_short: bool) -> Vec<Rule> {
    let mut rules = LONG_RULES.to_vec();
    if allow_short {
        rules.extend(LONG_RULES.iter().map(mirror));
    }
    rules
}

/// The result of evaluating every rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Firing {
    /// Actions of the rules that fired.
    pub actions: ActionSet,
    /// Bit `RuleId::index()` set for every rule that fired.
    pub rules: u32,
}

/// Evaluates **every** rule in `rules`; none short-circuits another.
pub fn fire(rules: &[Rule], macros: &MacroSet, micros: &MicroSet) -> Firing {
    let mut firing = Firing::default();
    for rule in rules {
        if rule.fires(macros, micros) {
            firing.actions.insert(rule.action);
            firing.rules |= 1 << rule.id.index();
        }
    }
    firing
}

/// Which conflict, if any, the resolution resolved to a wait (§8.4 steps 4–6).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ConflictKind {
    /// No conflict.
    #[default]
    None,
    /// Entry and exit on one axis (step 4 or 5).
    WithinAxis,
    /// Long entry and short entry together (step 6).
    Directional,
    /// Both a within-axis and a directional conflict.
    Both,
}

/// The vote of one agent on both axes, each in `{-1, 0, +1}`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AgentVote {
    /// Long-axis vote.
    pub long: i8,
    /// Short-axis vote.
    pub short: i8,
    /// The conflict this evaluation ended in.
    pub conflict: ConflictKind,
}

impl AgentVote {
    /// The wait vote `(0, 0)` without conflict.
    pub const WAIT: AgentVote = AgentVote {
        long: 0,
        short: 0,
        conflict: ConflictKind::None,
    };
}

fn axis(entry: bool, exit: bool) -> (i8, bool) {
    match (entry, exit) {
        (true, false) => (1, false),
        (false, true) => (-1, false),
        (true, true) => (0, true),
        (false, false) => (0, false),
    }
}

/// Resolves the fired actions into a vote, explicitly (§8.4 steps 2–7). The cold-feed case
/// (step 1) is the caller's.
///
/// - an empty micro set vetoes everything (noise);
/// - entry and exit on one axis give 0 on that axis;
/// - long entry and short entry give `(0, 0)`;
/// - with `allow_short = false` the short vote is always 0.
pub fn resolve(actions: ActionSet, micro_empty: bool, allow_short: bool) -> AgentVote {
    if micro_empty {
        return AgentVote::WAIT;
    }
    let (long, long_conflict) = axis(
        actions.contains(Action::LongEntry),
        actions.contains(Action::LongExit),
    );
    let (short, short_conflict, directional) = if allow_short {
        let (short, conflict) = axis(
            actions.contains(Action::ShortEntry),
            actions.contains(Action::ShortExit),
        );
        let directional =
            actions.contains(Action::LongEntry) && actions.contains(Action::ShortEntry);
        (short, conflict, directional)
    } else {
        (0, false, false)
    };
    let within = long_conflict || short_conflict;
    let conflict = match (within, directional) {
        (false, false) => ConflictKind::None,
        (true, false) => ConflictKind::WithinAxis,
        (false, true) => ConflictKind::Directional,
        (true, true) => ConflictKind::Both,
    };
    if directional {
        return AgentVote {
            long: 0,
            short: 0,
            conflict,
        };
    }
    AgentVote {
        long,
        short,
        conflict,
    }
}

/// The full outcome of one agent evaluation, for the vote table and the audit.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Evaluation {
    /// The resolved vote.
    pub vote: AgentVote,
    /// Bit `RuleId::index()` set for every rule that fired (before the noise veto).
    pub fired_rules: u32,
    /// The micro set was empty (noise veto applied).
    pub noise: bool,
    /// The feed had no indicator snapshot (cold): vote `(0, 0)`, no rule evaluated.
    pub cold: bool,
}

/// Evaluates one agent: macro stage, micro stage, every rule of `rules`, explicit resolution
/// (§8). `window` is the feed's closed bars, index 0 = last closed; `indicators` is `None` while
/// the feed is cold.
pub fn evaluate_agent(
    genome: &Genome,
    indicators: Option<&IndicatorSnapshot>,
    window: &[FeedBar],
    rules: &[Rule],
    allow_short: bool,
) -> Evaluation {
    let Some(indicators) = indicators else {
        return Evaluation {
            cold: true,
            ..Evaluation::default()
        };
    };
    let macros = macro_states(genome, indicators);
    let micros = micro_patterns(genome, window, indicators.atr);
    let firing = fire(rules, &macros, &micros);
    Evaluation {
        vote: resolve(firing.actions, micros.is_empty(), allow_short),
        fired_rules: firing.rules,
        noise: micros.is_empty(),
        cold: false,
    }
}

#[cfg(test)]
mod tests {
    use super::super::agent::tests::{bar, neutral_snapshot, reference_genome};
    use super::*;
    use rand::rngs::StdRng;
    use rand::{Rng, SeedableRng};
    use rust_decimal::Decimal;
    use rust_decimal_macros::dec;

    fn rule(name: RuleName) -> Rule {
        LONG_RULES[name as usize]
    }

    /// The minimal input on which each original rule fires.
    fn minimal(name: RuleName) -> (MacroSet, MicroSet) {
        let m = MacroSet::of;
        let u = MicroSet::of;
        match name {
            RuleName::E1 => (m(&[Bullish, High]), u(&[(PinBar, Up)])),
            RuleName::E2 => (m(&[Bullish]), u(&[(BreakOfStructure, Up)])),
            RuleName::E3 => (m(&[Oversold, Stretched]), u(&[(Engulfing, Up)])),
            RuleName::E4 => (m(&[Bullish, Low]), u(&[(InsideBar, Neutral)])),
            RuleName::E5 => (m(&[Bullish, High]), u(&[(BreakOfStructure, Up)])),
            RuleName::X1 => (m(&[Overbought]), u(&[(PinBar, Down)])),
            RuleName::X2 => (m(&[Stretched]), u(&[(InsideBar, Neutral)])),
            RuleName::X3 => (m(&[Bearish]), MicroSet::default()),
            RuleName::X4 => (m(&[Low, Overbought]), MicroSet::default()),
        }
    }

    #[test]
    fn the_table_matches_the_spec_and_the_mirrors_are_generated() {
        let expected = [
            (
                "E1",
                "Bullish ∧ High ∧ (PinBar↑ ∨ Engulfing↑)",
                "Bearish ∧ High ∧ (PinBar↓ ∨ Engulfing↓)",
            ),
            ("E2", "Bullish ∧ BoS↑", "Bearish ∧ BoS↓"),
            (
                "E3",
                "Oversold ∧ Stretched ∧ (PinBar↑ ∨ Engulfing↑)",
                "Overbought ∧ Stretched ∧ (PinBar↓ ∨ Engulfing↓)",
            ),
            (
                "E4",
                "Bullish ∧ Low ∧ InsideBar",
                "Bearish ∧ Low ∧ InsideBar",
            ),
            (
                "E5",
                "Bullish ∧ High ∧ ¬Overbought ∧ (BoS↑ ∨ Engulfing↑)",
                "Bearish ∧ High ∧ ¬Oversold ∧ (BoS↓ ∨ Engulfing↓)",
            ),
            (
                "X1",
                "Overbought ∧ (PinBar↓ ∨ BoS↓)",
                "Oversold ∧ (PinBar↑ ∨ BoS↑)",
            ),
            ("X2", "Stretched ∧ InsideBar", "Stretched ∧ InsideBar"),
            ("X3", "Bearish", "Bullish"),
            ("X4", "Low ∧ Overbought", "Low ∧ Oversold"),
        ];
        assert_eq!(LONG_RULES.len(), expected.len());
        for (rule, (id, original, mirrored)) in LONG_RULES.iter().zip(expected) {
            assert_eq!(rule.id.to_string(), id);
            assert_eq!(rule.describe(), original);
            let image = mirror(rule);
            assert_eq!(image.id.to_string(), format!("{id}′"));
            assert_eq!(image.describe(), mirrored);
            assert_eq!(image.action, rule.action.mirror());
            assert_eq!(mirror(&image), *rule);
            let expected_action = if id.starts_with('E') {
                Action::LongEntry
            } else {
                Action::LongExit
            };
            assert_eq!(rule.action, expected_action);
        }
        let ids = RuleId::all();
        for (index, id) in ids.iter().enumerate() {
            assert_eq!(id.index(), index);
        }
    }

    #[test]
    fn every_rule_and_its_mirror_fire_on_their_minimal_input() {
        for original_rule in LONG_RULES {
            let (macros, micros) = minimal(original_rule.id.name);
            assert!(
                original_rule.fires(&macros, &micros),
                "{}",
                original_rule.id
            );
            let image = mirror(original_rule);
            assert!(
                image.fires(&macros.mirror(), &micros.mirror()),
                "{}",
                image.id
            );
            // Inside the full book, too: every rule is evaluated.
            let firing = fire(&rule_book(true), &macros, &micros);
            assert_ne!(firing.rules & (1 << original_rule.id.index()), 0);
            assert!(firing.actions.contains(original_rule.action));
        }
    }

    #[test]
    fn e5_does_not_fire_when_overbought() {
        let (mut macros, micros) = minimal(RuleName::E5);
        macros.insert(Overbought);
        assert!(!rule(RuleName::E5).fires(&macros, &micros));
    }

    #[test]
    fn x2_and_its_mirror_fire_together() {
        let (macros, micros) = minimal(RuleName::X2);
        let firing = fire(&rule_book(true), &macros, &micros);
        assert!(firing.actions.contains(Action::LongExit));
        assert!(firing.actions.contains(Action::ShortExit));
        let vote = resolve(firing.actions, micros.is_empty(), true);
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (-1, -1, ConflictKind::None)
        );
    }

    #[test]
    fn entry_and_exit_on_one_axis_give_zero_on_that_axis() {
        let vote = resolve(
            ActionSet::of(&[Action::LongEntry, Action::LongExit]),
            false,
            true,
        );
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (0, 0, ConflictKind::WithinAxis)
        );
        let vote = resolve(
            ActionSet::of(&[Action::ShortEntry, Action::ShortExit, Action::LongExit]),
            false,
            true,
        );
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (-1, 0, ConflictKind::WithinAxis)
        );
    }

    #[test]
    fn opposite_entries_give_a_wait_on_both_axes() {
        let vote = resolve(
            ActionSet::of(&[Action::LongEntry, Action::ShortEntry]),
            false,
            true,
        );
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (0, 0, ConflictKind::Directional)
        );
        let vote = resolve(
            ActionSet::of(&[Action::LongEntry, Action::LongExit, Action::ShortEntry]),
            false,
            true,
        );
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (0, 0, ConflictKind::Both)
        );
    }

    #[test]
    fn agreeing_directions_are_not_conflicts() {
        let cases = [
            (vec![Action::LongExit, Action::ShortEntry], (-1, 1)),
            (vec![Action::LongEntry, Action::ShortExit], (1, -1)),
            (vec![Action::LongExit, Action::ShortExit], (-1, -1)),
            (vec![Action::LongEntry], (1, 0)),
            (vec![Action::ShortEntry], (0, 1)),
        ];
        for (actions, (long, short)) in cases {
            let vote = resolve(ActionSet::of(&actions), false, true);
            assert_eq!(
                (vote.long, vote.short, vote.conflict),
                (long, short, ConflictKind::None)
            );
        }
    }

    #[test]
    fn the_noise_veto_overrides_every_rule() {
        let vote = resolve(
            ActionSet::of(&[Action::LongExit, Action::ShortEntry]),
            true,
            true,
        );
        assert_eq!(vote, AgentVote::WAIT);
        // Bearish alone fires the macro-only X3; with no pattern the vote is still (0, 0).
        let mut snapshot = neutral_snapshot();
        snapshot.alma_slope = dec!(-1);
        let flat = bar(dec!(10), dec!(10), dec!(10), dec!(10));
        let evaluation = evaluate_agent(
            &reference_genome(),
            Some(&snapshot),
            &[flat],
            &rule_book(true),
            true,
        );
        assert!(evaluation.noise);
        assert_ne!(
            evaluation.fired_rules & (1 << rule(RuleName::X3).id.index()),
            0
        );
        assert_eq!(evaluation.vote, AgentVote::WAIT);
    }

    #[test]
    fn a_cold_feed_votes_wait_without_evaluating_rules() {
        let evaluation = evaluate_agent(&reference_genome(), None, &[], &rule_book(true), true);
        assert!(evaluation.cold);
        assert_eq!(evaluation.vote, AgentVote::WAIT);
        assert_eq!(evaluation.fired_rules, 0);
    }

    fn random_input(rng: &mut StdRng) -> (MacroSet, MicroSet) {
        let mut macros = MacroSet::default();
        for state in MacroState::ALL {
            if rng.gen_bool(0.4) {
                macros.insert(state);
            }
        }
        let mut micros = MicroSet::default();
        for pattern in Pattern::ALL {
            for direction in [Up, Down, Neutral] {
                if rng.gen_bool(0.25) {
                    micros.insert(pattern, direction);
                }
            }
        }
        (macros, micros)
    }

    fn vote_with(
        rules: &[Rule],
        macros: &MacroSet,
        micros: &MicroSet,
        allow_short: bool,
    ) -> (AgentVote, u32) {
        let firing = fire(rules, macros, micros);
        (
            resolve(firing.actions, micros.is_empty(), allow_short),
            firing.rules,
        )
    }

    #[test]
    fn permuting_the_rule_table_never_changes_a_vote() {
        let mut permutations: Vec<Vec<Rule>> = (0..LONG_RULES.len())
            .map(|k| {
                let mut rules = LONG_RULES.to_vec();
                rules.rotate_left(k);
                rules
            })
            .collect();
        permutations.push(LONG_RULES.iter().rev().copied().collect());
        let mut rng = StdRng::seed_from_u64(500);
        for _ in 0..500 {
            let (macros, micros) = random_input(&mut rng);
            for allow_short in [true, false] {
                let baseline = vote_with(&rule_book(allow_short), &macros, &micros, allow_short);
                for permutation in &permutations {
                    let mut rules = permutation.clone();
                    if allow_short {
                        rules.extend(permutation.iter().map(mirror));
                    }
                    assert_eq!(vote_with(&rules, &macros, &micros, allow_short), baseline);
                }
            }
        }
    }

    #[test]
    fn reflecting_the_rule_inputs_swaps_the_axes() {
        let rules = rule_book(true);
        let mut rng = StdRng::seed_from_u64(500);
        for _ in 0..500 {
            let (macros, micros) = random_input(&mut rng);
            let (vote, _) = vote_with(&rules, &macros, &micros, true);
            let (reflected, _) = vote_with(&rules, &macros.mirror(), &micros.mirror(), true);
            assert_eq!((reflected.long, reflected.short), (vote.short, vote.long));
            assert_eq!(reflected.conflict, vote.conflict);
        }
    }

    fn price(rng: &mut StdRng) -> Decimal {
        Decimal::new(rng.gen_range(9_000..11_000), 2)
    }

    /// A random candle with a non-zero body (see the doji note in the report of §8.2).
    fn random_bar(rng: &mut StdRng) -> FeedBar {
        let open = price(rng);
        let mut close = price(rng);
        if close == open {
            close += dec!(0.01);
        }
        let high = open.max(close) + Decimal::new(rng.gen_range(0..300), 2);
        let low = open.min(close) - Decimal::new(rng.gen_range(0..300), 2);
        bar(open, high, low, close)
    }

    fn reflect_bar(b: &FeedBar, centre: Decimal) -> FeedBar {
        bar(
            centre - b.open,
            centre - b.low,
            centre - b.high,
            centre - b.close,
        )
    }

    #[test]
    fn reflecting_indicators_and_candles_swaps_the_axes_of_an_agent() {
        let rules = rule_book(true);
        let mut genome = reference_genome();
        let mut rng = StdRng::seed_from_u64(500);
        for _ in 0..500 {
            genome.rsi_overbought = Decimal::from(rng.gen_range(55..85));
            genome.rsi_oversold = Decimal::from(rng.gen_range(15..45));
            genome.bos_margin = Decimal::new(rng.gen_range(-20..20), 2);
            // The reflected agent: RSI thresholds reflect with the RSI.
            let mut reflected_genome = genome;
            reflected_genome.rsi_overbought = dec!(100) - genome.rsi_oversold;
            reflected_genome.rsi_oversold = dec!(100) - genome.rsi_overbought;

            let snapshot = IndicatorSnapshot {
                rsi: Decimal::from(rng.gen_range(0..=100)),
                atr: Decimal::new(rng.gen_range(10..300), 2),
                atr_ratio: Decimal::new(rng.gen_range(30..200), 2),
                alma_slope: Decimal::new(rng.gen_range(-20..20), 2),
                vwap_dev: Decimal::new(rng.gen_range(0..200), 2),
            };
            let reflected_snapshot = IndicatorSnapshot {
                rsi: dec!(100) - snapshot.rsi,
                alma_slope: -snapshot.alma_slope,
                ..snapshot
            };
            let window: Vec<FeedBar> = (0..rng.gen_range(1..6))
                .map(|_| random_bar(&mut rng))
                .collect();
            let reflected_window: Vec<FeedBar> =
                window.iter().map(|b| reflect_bar(b, dec!(200))).collect();

            let vote = evaluate_agent(&genome, Some(&snapshot), &window, &rules, true).vote;
            let reflected = evaluate_agent(
                &reflected_genome,
                Some(&reflected_snapshot),
                &reflected_window,
                &rules,
                true,
            )
            .vote;
            assert_eq!((reflected.long, reflected.short), (vote.short, vote.long));
            assert_eq!(reflected.conflict, vote.conflict);
        }
    }

    #[test]
    fn long_only_never_votes_short_and_never_evaluates_mirrors() {
        let rules = rule_book(false);
        assert!(rules.iter().all(|r| !r.id.mirrored));
        let mut rng = StdRng::seed_from_u64(500);
        for _ in 0..500 {
            let (macros, micros) = random_input(&mut rng);
            let (vote, fired) = vote_with(&rules, &macros, &micros, false);
            assert_eq!(vote.short, 0);
            assert_eq!(fired >> (RULE_COUNT / 2), 0);
            assert_ne!(vote.conflict, ConflictKind::Directional);
        }
        let vote = resolve(
            ActionSet::of(&[Action::ShortEntry, Action::LongEntry]),
            false,
            false,
        );
        assert_eq!(
            (vote.long, vote.short, vote.conflict),
            (1, 0, ConflictKind::None)
        );
    }
}
