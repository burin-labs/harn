//! Guaranteed fields of native transcript-event producers.

use crate::{ShapeFieldDescriptor as Field, Ty, TY_BOOL, TY_INT, TY_STRING};

const EVENT_FIELDS: Ty = Ty::Shape(&[
    Field::new("id", TY_STRING),
    Field::new("kind", TY_STRING),
    Field::new("role", TY_STRING),
    Field::new("visibility", TY_STRING),
    Field::new("text", TY_STRING),
    Field::new(
        "blocks",
        Ty::Apply(
            "list",
            &[Ty::Shape(&[
                Field::new("type", Ty::LitString("text")),
                Field::new("text", TY_STRING),
                Field::new("visibility", TY_STRING),
            ])],
        ),
    ),
]);

pub const SYSTEM_REMINDER: Ty = Ty::Shape(&[
    Field::new("id", TY_STRING),
    Field::new("tags", Ty::Apply("list", &[TY_STRING])),
    Field::optional("dedupe_key", TY_STRING),
    Field::optional("ttl_turns", TY_INT),
    Field::new("preserve_on_compact", TY_BOOL),
    Field::new("propagate", TY_STRING),
    Field::new("role_hint", TY_STRING),
    Field::new("authority", TY_STRING),
    Field::new("source", TY_STRING),
    Field::new("body", TY_STRING),
    Field::new("fired_at_turn", TY_INT),
    Field::optional("originating_agent_id", TY_STRING),
    Field::optional("goal_pin", Ty::Named("unknown")),
]);

pub const TRANSCRIPT_REMINDER_EVENT: Ty = Ty::OpenShape(
    &[
        Field::new("reminder", SYSTEM_REMINDER),
        Field::new("metadata", SYSTEM_REMINDER),
    ],
    &[EVENT_FIELDS],
);

// These three producers retain arbitrary JSON payloads rather than normalizing
// them. Consumers validate the payload before requiring lifecycle-specific keys.
pub const TRANSCRIPT_SUSPENSION_EVENT: Ty = Ty::OpenShape(
    &[
        Field::new("suspension", Ty::Named("unknown")),
        Field::new("metadata", Ty::Named("unknown")),
    ],
    &[EVENT_FIELDS],
);

pub const TRANSCRIPT_RESUMPTION_EVENT: Ty = Ty::OpenShape(
    &[
        Field::new("resumption", Ty::Named("unknown")),
        Field::new("metadata", Ty::Named("unknown")),
    ],
    &[EVENT_FIELDS],
);

pub const TRANSCRIPT_DRAIN_DECISION_EVENT: Ty = Ty::OpenShape(
    &[
        Field::new("drain", Ty::Named("unknown")),
        Field::new("metadata", Ty::Named("unknown")),
    ],
    &[EVENT_FIELDS],
);
