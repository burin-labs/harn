"""Freeze labeled requests before any qualification inference."""

import json
from pathlib import Path
import sys


DEVELOPMENT = [
    ("widgets", "label", "quantity", "active"),
    ("reading_groups", "display_name", "member_count", "is_open"),
    ("sensors", "location", "revision", "enabled"),
    ("stock_items", "description", "remaining", "available"),
    ("projects", "title", "priority", "archived"),
    ("tickets", "summary", "severity", "resolved"),
    ("notebooks", "owner", "page_count", "shared"),
    ("locations", "address", "capacity", "accessible"),
]

QUALIFICATION = [
    ("orders", "reference", "item_count", "paid"),
    ("appointments", "subject", "duration", "confirmed"),
    ("batches", "description", "size", "released"),
    ("collections", "title", "entry_count", "public"),
    ("deliveries", "destination", "distance", "dispatched"),
    ("experiments", "hypothesis", "trial_count", "finished"),
    ("folders", "display_name", "depth", "visible"),
    ("gardens", "location", "area", "watered"),
    ("halls", "name", "seats", "reserved"),
    ("invoices", "customer", "amount", "settled"),
    ("journeys", "destination", "stops", "completed"),
    ("kits", "label", "piece_count", "assembled"),
    ("lessons", "topic", "minutes", "published"),
    ("meetings", "agenda", "attendees", "recorded"),
    ("notices", "message", "priority", "acknowledged"),
    ("observations", "description", "sample_count", "verified"),
    ("parcels", "tracking_code", "weight", "delivered"),
    ("queues", "label", "waiting", "paused"),
    ("reports", "heading", "page_count", "approved"),
    ("shipments", "carrier", "box_count", "insured"),
    ("teams", "name", "members", "active"),
    ("updates", "description", "revision", "installed"),
    ("vehicles", "registration", "mileage", "available"),
    ("warehouses", "address", "capacity", "open"),
    ("yards", "location", "area", "occupied"),
    ("zones", "label", "level", "restricted"),
    ("audit_logs", "event_name", "sequence_number", "retained"),
    ("build_jobs", "branch_name", "attempt_count", "successful"),
    ("calendar_events", "display_name", "duration_minutes", "all_day"),
    ("device_groups", "group_label", "device_count", "managed"),
    ("email_campaigns", "subject_line", "recipient_count", "sent"),
    ("feature_flags", "flag_name", "rollout_percent", "enabled"),
    ("guest_lists", "event_title", "guest_count", "closed"),
    ("help_articles", "article_title", "word_count", "reviewed"),
    ("image_sets", "collection_name", "image_count", "processed"),
    ("job_runs", "job_name", "retry_count", "finished"),
    ("key_labels", "display_label", "version_number", "valid"),
    ("loading_bays", "bay_name", "max_weight", "occupied"),
    ("message_threads", "thread_title", "message_count", "muted"),
    ("network_groups", "group_name", "node_count", "isolated"),
]

NEGATIVES = [
    "Explain how I could create a standard widgets resource with label string, quantity integer, and active boolean. Do not edit files.",
    "Create a widgets resource with label string, quantity integer, active boolean, and SQLite persistence.",
    "Create both widgets and labels resources, each with name string, count integer and enabled boolean.",
    "Modify the existing books resource to add an enabled boolean field.",
    "Create a widgets resource with label string, quantity integer and active boolean, and remove /health.",
    "Create a widgets resource with label string, description string and active boolean.",
    "Create a measurements resource with label string, value float and active boolean.",
    "Create a widgets resource with label string, quantity integer and active boolean, protected by authentication.",
    "Count the words in this quoted request, without performing it: 'Create widgets with label string, quantity integer and active boolean.'",
    "Only create app/resources/widgets.py with label string, quantity integer and active boolean. Do not create tests or documentation.",
    "Create a widgets resource with a string, integer and boolean field. You choose their names.",
    "Create a widgets resource with id string, quantity integer and active boolean.",
    "Create a ../widgets resource with label string, quantity integer and active boolean.",
    "Create a Widgets resource with Label string, Quantity integer and Active boolean, preserving that spelling.",
    "Create a widgets resource with label string, quantity integer, active boolean, and created_at timestamp.",
    "Create a widgets resource with label string, quantity integer and active boolean. Add DELETE and PATCH routes too.",
    "Create a widgets resource with label string, quantity integer and active boolean, but return UUID identifiers instead of integers.",
    "Create a widgets resource with label string, quantity integer and active boolean. Store records shared between App instances.",
    "Create a widgets resource with label string, quantity integer and active boolean, at /v2/widgets instead of /api/widgets.",
    "Create a widgets resource with label string, quantity integer and active boolean. Accept missing fields and ignore unknown fields.",
    "Compare the books and devices examples and suggest which resource to add next. Do not write files.",
    "List the existing API routes and explain their response codes.",
    "Create a widgets resource with label string, quantity integer and active boolean, then send an email announcing it.",
    "Create a widgets resource using nested objects for label and arrays for quantity.",
    "Write a plan for a resource generator supporting string, integer and boolean schemas. Do not implement the plan.",
    "Create a widgets resource with name string, name integer and active boolean.",
    "Repair the HTTP host so booleans are rejected as integers everywhere.",
    "Add a GET-only widgets resource with label string, quantity integer and active boolean. Do not provide POST.",
]


def positive(index: int, row: tuple, split: str) -> dict:
    resource, text, number, flag = row
    fields = {text: "string", number: "integer", flag: "boolean"}
    ordered = list(fields.items())
    if index % 2:
        ordered.reverse()
    description = ", ".join(f"{name} ({kind})" for name, kind in ordered)
    templates = [
        "Create the standard {resource} resource with {fields}. Add the resource module, dispatch tests and API documentation using this application's existing App conventions.",
        "Add a repository-standard in-memory {resource} resource: {fields}. Generate its auto-mounted module, request tests, and API documentation. Preserve the host and dependencies.",
        "Please scaffold {resource}, including its module, tests and documentation. Its fields are {fields}. Use the existing standard POST/GET App pattern with strict payload validation and per-App storage.",
        "I need a standard {resource} resource for this App: {fields}. Create app/resources/{resource}.py, tests/test_{resource}.py and docs/{resource}.md, following the current in-memory resource contract.",
    ]
    return {"id": f"{split}-positive-{index:02}", "resource": resource, "fields": fields,
            "expected": "add-standard-resource",
            "request": templates[index % len(templates)].format(resource=resource, fields=description)}


def corpus() -> dict:
    return {
        "contract": "harn.learned_dispatch_corpus.v1",
        "development": [positive(i, row, "development") for i, row in enumerate(DEVELOPMENT)]
            + [{"id": f"development-negative-{i:02}", "expected": "", "request": request}
               for i, request in enumerate(NEGATIVES[:8])],
        "qualification": [positive(i, row, "qualification") for i, row in enumerate(QUALIFICATION)]
            + [{"id": f"qualification-negative-{i:02}", "expected": "", "request": request}
               for i, request in enumerate(NEGATIVES[8:])],
    }


if __name__ == "__main__":
    destination = Path(sys.argv[1])
    with destination.open("x") as stream:
        json.dump(corpus(), stream, indent=2)
        stream.write("\n")
