// SPDX-License-Identifier: Apache-2.0
//! Property tests (T-25): for every registry action, any argument values that
//! `Action::from_call` accepts must format and re-parse to an equal value, and hostile
//! text must never panic the parser.

use std::cell::Cell;

use apoth_action::text::Call;
use apoth_action::{Action, ParamKind, REGISTRY};
use proptest::prelude::*;

fn value(kind: ParamKind) -> BoxedStrategy<String> {
    let text = prop_oneof![
        "[a-zA-Z0-9 _./:-]{0,12}",
        any::<String>().prop_map(|s| s.chars().take(16).collect()),
        Just("a;b".to_string()),
        Just("quo\"te\\".to_string()),
        Just("{selection}".to_string()),
        Just("{{x}}".to_string()),
    ];
    match kind {
        ParamKind::Text | ParamKind::Char | ParamKind::Keys => prop_oneof![
            text,
            Just("ctrl+a".to_string()),
            Just("x".to_string()),
            Just("shift+Return".to_string()),
        ]
        .boxed(),
        ParamKind::Bool => any::<bool>().prop_map(|b| b.to_string()).boxed(),
        ParamKind::Int => any::<i32>().prop_map(|n| n.to_string()).boxed(),
        ParamKind::Uint => (0u32..100_000).prop_map(|n| n.to_string()).boxed(),
        ParamKind::Float => (-1000i32..1000).prop_map(|n| format!("{}.5", n)).boxed(),
        ParamKind::Duration => (0u32..100_000).prop_map(|n| format!("{n}ms")).boxed(),
        ParamKind::Hex => proptest::collection::vec(any::<u8>(), 0..16)
            .prop_map(|b| b.iter().map(|x| format!("{x:02x}")).collect())
            .boxed(),
        ParamKind::Enum(names) => proptest::sample::select(names.to_vec())
            .prop_map(str::to_string)
            .boxed(),
        ParamKind::Argv => Just(String::new()).boxed(),
    }
}

fn argv() -> impl Strategy<Value = Vec<String>> {
    let el = prop_oneof![
        "[a-z]{1,6}",
        Just("{url}".to_string()),
        Just("{cwd}".to_string()),
        Just("{{lit}}".to_string()),
        "[ -~]{0,8}",
    ];
    ("[a-z]{1,6}", proptest::collection::vec(el, 0..4)).prop_map(|(first, mut rest)| {
        rest.insert(0, first);
        rest
    })
}

fn call_strategy() -> impl Strategy<Value = Call> {
    (0..REGISTRY.len()).prop_flat_map(|i| {
        let info = &REGISTRY[i];
        let params: Vec<_> = info
            .params
            .iter()
            .map(|p| {
                let name = p.name;
                let kind = p.kind;
                let required = p.required;
                (any::<bool>(), value(kind), argv())
                    .prop_map(move |(on, v, av)| (name, kind, required || on, v, av))
            })
            .collect();
        params.prop_map(move |ps| {
            let mut c = Call::new(info.id);
            for (name, kind, on, v, av) in ps {
                if !on {
                    continue;
                }
                if kind == ParamKind::Argv {
                    c.argv = Some(av);
                } else {
                    c.named.push((name.to_string(), v));
                }
            }
            c
        })
    })
}

thread_local! {
    static ACCEPTED: Cell<u32> = const { Cell::new(0) };
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(2000))]

    #[test]
    fn accepted_actions_roundtrip(call in call_strategy()) {
        if let Ok(a) = Action::from_call(&call) {
            ACCEPTED.with(|c| c.set(c.get() + 1));
            let text = a.to_call().format();
            let b = Action::parse(&text).unwrap_or_else(|e| panic!("`{text}`: {e}"));
            prop_assert_eq!(&a, &b);
            prop_assert_eq!(b.to_call().format(), text);
            // classification never panics and is stable
            prop_assert_eq!(a.classify_closed(), b.classify_closed());
        }
    }

    #[test]
    fn hostile_text_never_panics(s in any::<String>()) {
        let _ = Action::parse(&s);
        let _ = Action::parse_script(&s);
    }
}

#[test]
fn property_is_not_vacuous() {
    // run the strategy by hand: a healthy fraction of generated calls must be accepted
    use proptest::strategy::ValueTree;
    use proptest::test_runner::TestRunner;
    let mut runner = TestRunner::deterministic();
    let strat = call_strategy();
    let (mut ok, n) = (0, 500);
    for _ in 0..n {
        let call = strat.new_tree(&mut runner).unwrap().current();
        if Action::from_call(&call).is_ok() {
            ok += 1;
        }
    }
    assert!(ok > n / 4, "only {ok}/{n} generated calls were accepted");
}
