use rc_sql::{Hit, Index, Key, ObjKind, RefKind, Severity, Space, analyze};
use std::path::{Path, PathBuf};
use std::sync::Arc;

const CRM: &str = "--liquibase formatted sql

--changeset ada:20260101-01
--comment: Customers of the shop
create schema if not exists crm;
create table crm.customers (
    id uuid primary key,
    name text not null,
    credit numeric(12, 2) default 0
);

--changeset ada:20260101-02
alter table crm.customers add column email text;
create index ix_customers_name on crm.customers (name);
";

const ORDERS: &str = "--liquibase formatted sql

--changeset bob:20260102-01
create table orders.orders (
    id uuid primary key,
    customer_id uuid not null references crm.customers (id),
    status text not null
);

--changeset bob:20260102-02
--comment: Places an order
create or replace function orders_app.place(p_customer uuid, p_note text default null)
returns uuid
language plpgsql
set search_path = orders, crm
as $$
declare
    v_id uuid := gen_random_uuid();
    v_row orders.orders%rowtype;
begin
    if not exists (select 1 from customers where id = p_customer) then
        raise exception 'no customer' using errcode = 'U1404';
    end if;
    insert into orders.orders (id, customer_id, status) values (v_id, p_customer, 'new');
    perform orders_app.audit(v_id);
    return v_id;
end
$$;

--changeset bob:20260102-03
create procedure orders_app.audit(p_id uuid) language sql as $$
    update orders.orders set status = status where id = p_id;
$$;
create procedure orders_app.audit(p_id uuid, p_note text) language sql as $$ select 1 $$;

--changeset bob:20260102-04
create view orders_app.orders_v as select o.id, c.name as customer from orders.orders o join crm.customers c on c.id = o.customer_id;
create trigger tr_orders after insert on orders.orders for each row execute function orders_app.on_insert();
call orders_app.audit('00000000-0000-0000-0000-000000000000', 'x');
";

fn index() -> Index {
    Index::new(vec![(PathBuf::from("db/01-crm.sql"), Arc::new(analyze(CRM))), (PathBuf::from("db/02-orders.sql"), Arc::new(analyze(ORDERS)))])
}

fn offset_of(text: &str, needle: &str, nth: usize) -> usize {
    text.match_indices(needle).nth(nth).unwrap_or_else(|| panic!("{needle} #{nth}")).0
}

#[test]
fn definitions_with_columns_signatures_and_changesets() {
    let fa = analyze(CRM);
    assert!(fa.problems.is_empty(), "{:?}", fa.problems);
    let kinds: Vec<(ObjKind, String, bool)> = fa.definitions.iter().map(|d| (d.kind, d.key.qualified(), d.is_alter)).collect();
    assert_eq!(
        kinds,
        [
            (ObjKind::Schema, "crm".to_string(), false),
            (ObjKind::Table, "crm.customers".into(), false),
            (ObjKind::Table, "crm.customers".into(), true),
            (ObjKind::Index, "crm.ix_customers_name".into(), false)
        ]
    );
    let table = &fa.definitions[1];
    assert_eq!(&CRM[table.name.clone()], "crm.customers");
    assert_eq!(table.changeset.as_deref(), Some("ada:20260101-01"));
    assert_eq!(table.comment, "Customers of the shop");
    let cols: Vec<(&str, &str, bool)> = table.columns.iter().map(|c| (c.name.as_str(), c.ty.as_str(), c.not_null)).collect();
    assert_eq!(cols, [("id", "uuid", true), ("name", "text", true), ("credit", "numeric(12, 2)", false)]);

    let fa = analyze(ORDERS);
    assert!(fa.problems.is_empty(), "{:?}", fa.problems);
    let place = fa.definitions.iter().find(|d| d.key.name == "place").unwrap();
    assert_eq!(place.kind, ObjKind::Function);
    assert_eq!(place.key.signature.as_deref(), Some("uuid,text"));
    assert_eq!(place.signature(), "orders_app.place(p_customer uuid, p_note text) RETURNS uuid");
    assert_eq!(place.arity(), (1, Some(2)));
    assert_eq!(place.language.as_deref(), Some("plpgsql"));
    let audits = fa.definitions.iter().filter(|d| d.key.name == "audit" && d.kind == ObjKind::Procedure).count();
    assert_eq!(audits, 2);
    let view = fa.definitions.iter().find(|d| d.kind == ObjKind::View).unwrap();
    assert_eq!(view.columns.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "customer"]);
    let trigger = fa.definitions.iter().find(|d| d.kind == ObjKind::Trigger).unwrap();
    assert_eq!(trigger.key.signature.as_deref(), Some("orders"));
}

#[test]
fn references_know_what_they_do() {
    let fa = analyze(ORDERS);
    let has = |name: &str, kind: RefKind| fa.references.iter().any(|r| r.name.to_string() == name && r.kind == kind);
    assert!(has("crm.customers", RefKind::ForeignKey));
    assert!(has("orders.orders", RefKind::Write), "insert in the PL/pgSQL body");
    assert!(has("customers", RefKind::Read), "unqualified, resolved by the function's search_path");
    assert!(has("orders_app.audit", RefKind::Call));
    assert!(has("orders.orders", RefKind::Type), "%rowtype in DECLARE");
    assert!(has("orders_app.on_insert", RefKind::Trigger));
    assert!(has("orders.orders", RefKind::Trigger));
    let update = fa.references.iter().find(|r| r.name.to_string() == "orders.orders" && r.kind == RefKind::Write && r.from.as_ref().is_some_and(|k| k.name == "audit")).expect("write in the SQL body");
    assert_eq!(&ORDERS[update.span.clone()], "orders.orders");
}

#[test]
fn the_index_resolves_across_files_and_overloads() {
    let index = index();
    let customers = Key::new(Space::Relation, "crm", "customers");
    // Through the function's search_path (orders, crm): `customers` is crm.customers.
    let at = offset_of(ORDERS, "from customers", 0) + 5;
    match index.at(Path::new("db/02-orders.sql"), at) {
        Some(Hit::Reference(r, keys)) => {
            assert_eq!(r.name.name, "customers");
            assert_eq!(keys, std::slice::from_ref(&customers));
        }
        other => panic!("{other:?}"),
    }
    let usages: Vec<(String, RefKind)> = index.usages(&customers).into_iter().map(|(p, r)| (p.display().to_string(), r.kind)).collect();
    assert!(usages.contains(&("db/01-crm.sql".into(), RefKind::Alter)));
    assert!(usages.contains(&("db/01-crm.sql".into(), RefKind::IndexOn)));
    assert!(usages.contains(&("db/02-orders.sql".into(), RefKind::ForeignKey)));
    assert!(usages.contains(&("db/02-orders.sql".into(), RefKind::Read)));
    // Columns after ALTER TABLE.
    assert_eq!(index.columns(&customers).iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["id", "name", "credit", "email"]);
    // One argument → audit(uuid); two → audit(uuid, text).
    let one = offset_of(ORDERS, "orders_app.audit(v_id)", 0);
    let Some(Hit::Reference(_, keys)) = index.at(Path::new("db/02-orders.sql"), one) else { panic!() };
    assert_eq!(keys.iter().map(|k| k.signature.clone().unwrap()).collect::<Vec<_>>(), ["uuid"]);
    let two = offset_of(ORDERS, "call orders_app.audit", 0) + 5;
    let Some(Hit::Reference(_, keys)) = index.at(Path::new("db/02-orders.sql"), two) else { panic!() };
    assert_eq!(keys.iter().map(|k| k.signature.clone().unwrap()).collect::<Vec<_>>(), ["uuid,text"]);
    // On a definition's name.
    let name = offset_of(ORDERS, "orders_app.place", 0) + 12;
    assert!(matches!(index.at(Path::new("db/02-orders.sql"), name), Some(Hit::Definition(k)) if k.name == "place"));
    // Builtins resolve to nothing.
    let builtin = offset_of(ORDERS, "gen_random_uuid", 0);
    assert!(matches!(index.at(Path::new("db/02-orders.sql"), builtin), Some(Hit::Reference(_, keys)) if keys.is_empty()));
}

#[test]
fn syntax_errors_stay_in_their_statement() {
    let text = "create table a (id int);\nselect from where;\ncreate table b (id int);\n";
    let fa = analyze(text);
    assert_eq!(fa.problems.len(), 1, "{:?}", fa.problems);
    let p = &fa.problems[0];
    assert_eq!((p.severity, p.code, &text[p.span.clone()]), (Severity::Error, "syntax", "where"));
    assert_eq!(fa.definitions.iter().map(|d| d.key.name.as_str()).collect::<Vec<_>>(), ["a", "b"]);
}

#[test]
fn plpgsql_errors_point_into_the_body() {
    let text = "create function s.f(p s.money) returns int language plpgsql as $$\ndeclare v s.big_type;\nbegin\n  selec 1;\n  return 1;\nend $$;\n";
    let fa = analyze(text);
    assert_eq!(fa.problems.len(), 1, "{:?}", fa.problems);
    let p = &fa.problems[0];
    assert_eq!((p.code, &text[p.span.clone()]), ("plpgsql", "selec"));
    let ok = analyze("create function s.f(p s.money) returns int language plpgsql as $$\ndeclare v s.big_type;\nbegin\n  return 1;\nend $$;\n");
    assert!(ok.problems.is_empty(), "types of other schemas are fine: {:?}", ok.problems);
}

#[test]
fn liquibase_properties_come_back_in_names() {
    let fa = analyze("--liquibase formatted sql\n--changeset a:1\ncreate table ${schema}.t (id int);\n");
    assert_eq!(fa.definitions[0].key.schema, "${schema}");
}
