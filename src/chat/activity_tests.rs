//! Pure regressions for caught-Pokémon task identities; no app or image loading required.
use super::activity::{assign_pokemon, assigned, roster};
use crate::{
    agent,
    pokemon::{Balance, Companion, EvoLine, EvoNode, Mon, Rarity, forms::Form},
};
use std::collections::{BTreeMap, HashMap};

fn line(species: u16) -> EvoLine {
    EvoLine {
        base_id: species,
        tree: EvoNode {
            species_id: species,
            children: vec![],
        },
        rarity: Rarity::Common,
        names: BTreeMap::from([(
            species,
            BTreeMap::from([("en".into(), format!("Species {species}"))]),
        )]),
    }
}

fn mon(id: &str, species: u16, shiny: bool, form: Option<&str>) -> Mon {
    let mut companion = Companion {
        egg_tokens: Balance::EGG_HATCH_THRESHOLD,
        ..Default::default()
    };
    companion.hatch(line(species));
    let mut mon = companion.active.take().expect("hatched individual");
    mon.profile
        .as_mut()
        .expect("hatch creates profile")
        .instance_id = id.into();
    mon.shiny = shiny;
    mon.form = form.map(|form| Form(form.into()));
    mon
}

fn task(id: &str) -> agent::Task {
    agent::Task {
        id: id.into(),
        kind: "local_agent".into(),
        description: "Same description".into(),
    }
}

#[test]
fn activity_roster_contains_collected_and_hatched_individuals_but_no_pending_egg() {
    let mut companion = Companion {
        pending_line: Some(line(1)),
        ..Default::default()
    };
    assert!(
        roster(&companion).is_empty(),
        "pending hatch is not a caught Pokémon"
    );
    companion.collection = vec![mon("caught", 201, true, Some("question"))];
    companion.active = Some(mon("growing", 25, false, None));
    let caught = roster(&companion);
    assert_eq!(caught.len(), 2);
    assert_eq!(caught[0].id, "caught");
    assert_eq!(
        caught[0].appearance,
        (201, true, Some(Form("question".into())))
    );
    assert!(caught[0].name.starts_with("Shiny "));
    assert_eq!(caught[1].id, "growing");
    assert_eq!(caught[1].appearance, (25, false, None));
}

#[test]
fn activity_assignments_preserve_individuals_through_task_and_roster_reordering() {
    let mut companion = Companion::default();
    companion.collection = vec![
        mon("first", 25, false, None),
        mon("second", 25, false, None),
        mon("third", 201, true, Some("a")),
    ];
    let caught = roster(&companion);
    let mut assignments = HashMap::new();
    assign_pokemon(
        &mut assignments,
        &[task("a"), task("b"), task("c")],
        &caught,
    );
    assert_ne!(
        assignments["a"], assignments["b"],
        "identical appearances are distinct individuals"
    );
    let original = assignments.clone();
    let mut reordered = caught.clone();
    reordered.reverse();
    assign_pokemon(&mut assignments, &[task("c"), task("a")], &reordered);
    assert_eq!(assignments["a"], original["a"]);
    assert_eq!(assignments["c"], original["c"]);
    assert!(!assignments.contains_key("b"));
    assign_pokemon(
        &mut assignments,
        &[task("c"), task("new"), task("a")],
        &reordered,
    );
    assert_eq!(
        assignments["new"], original["b"],
        "new task uses the freed individual"
    );
}

#[test]
fn activity_assignments_balance_overflow_and_leave_shells_unassigned() {
    let mut companion = Companion::default();
    companion.collection = vec![
        mon("one", 25, false, None),
        mon("two", 201, true, Some("question")),
    ];
    let caught = roster(&companion);
    let mut tasks: Vec<_> = (0..9).map(|i| task(&i.to_string())).collect();
    tasks.insert(
        0,
        agent::Task {
            kind: "local_bash".into(),
            ..task("shell")
        },
    );
    let mut assignments = HashMap::from([("shell".into(), "one".into())]);
    assign_pokemon(&mut assignments, &tasks, &caught);
    assert_eq!(assignments.len(), 9);
    assert!(assigned(&tasks[0], &assignments, &caught).is_none());
    let counts: Vec<_> = caught
        .iter()
        .map(|mon| assignments.values().filter(|id| *id == &mon.id).count())
        .collect();
    assert_eq!(counts.iter().sum::<usize>(), 9);
    assert!(counts[0].abs_diff(counts[1]) <= 1);
    for task in &tasks[1..] {
        let selected = assigned(task, &assignments, &caught).unwrap();
        assert_eq!(
            selected.appearance,
            caught
                .iter()
                .find(|p| p.id == assignments[&task.id])
                .unwrap()
                .appearance
        );
    }
}

#[test]
fn activity_roster_changes_update_appearance_without_reassigning_survivors() {
    let mut companion = Companion::default();
    companion.collection = vec![
        mon("survivor", 25, false, None),
        mon("removed", 201, false, Some("a")),
    ];
    let mut caught = roster(&companion);
    let tasks = [task("a"), task("b")];
    let mut assignments = HashMap::new();
    assign_pokemon(&mut assignments, &tasks, &caught);
    caught[0].appearance = (201, true, Some(Form("question".into())));
    caught[0].name = "Shiny evolved individual".into();
    caught.remove(1);
    companion.collection = vec![mon("replacement", 1, false, None)];
    caught.extend(roster(&companion));
    assign_pokemon(&mut assignments, &tasks, &caught);
    assert_eq!(assignments["a"], "survivor");
    assert_eq!(assignments["b"], "replacement");
    let survivor = assigned(&tasks[0], &assignments, &caught).unwrap();
    assert_eq!(
        survivor.appearance,
        (201, true, Some(Form("question".into())))
    );
    assert_eq!(survivor.name, "Shiny evolved individual");
    assign_pokemon(&mut assignments, &tasks, &[]);
    assert!(
        assignments.is_empty(),
        "empty or disabled roster clears old identities"
    );
    assert!(assigned(&tasks[0], &assignments, &[]).is_none());
}
