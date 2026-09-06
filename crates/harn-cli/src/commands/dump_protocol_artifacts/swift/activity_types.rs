use super::super::{activity::ActivityVocabulary, records::Target};
use super::swift_enum;

pub(super) fn append_activity_types(out: &mut String, activity: &ActivityVocabulary) {
    for (_, name, values) in activity.projections() {
        out.push_str(&swift_enum(name, values));
    }
    for record in &activity.records {
        record.append(out, Target::Swift);
    }
}
