// Unless explicitly stated otherwise all files in this repository are licensed under the
// Apache License Version 2.0.
// This product includes software developed at Datadog (https://www.datadoghq.com/).
// Copyright 2026-Present Datadog, Inc.

//! The public list of volatile functions, which `sqleq-lean` reads and `sqleq-fuzz` follows.

use sqleq_frontend::{is_volatile, VOLATILE_FUNCTIONS};

#[test]
fn the_list_is_spelled_as_postgres_spells_it_and_sorted() {
    for f in VOLATILE_FUNCTIONS {
        assert_eq!(*f, f.to_lowercase(), "{f} is lowercase");
    }
    assert!(VOLATILE_FUNCTIONS.windows(2).all(|w| w[0] < w[1]), "sorted, without duplicates");
}

#[test]
fn a_name_matches_in_any_case() {
    for f in ["random", "RANDOM", "Gen_Random_UUID", "nextval", "clock_timestamp", "uuidv7"] {
        assert!(is_volatile(f), "{f}");
    }
}

#[test]
fn stable_clocks_and_non_postgres_names_are_not_on_it() {
    // The statement-stable clocks are one value per statement, which a shared constant models
    // faithfully; `uuid` and `random_uuid` are not Postgres functions.
    for f in ["now", "statement_timestamp", "transaction_timestamp", "txid_current", "uuid", "random_uuid"] {
        assert!(!is_volatile(f), "{f}");
    }
}
