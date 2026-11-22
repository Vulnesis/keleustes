//! Host schedules on the wire (WP-G): `Board.SchedulePull` and
//! `Board.SchedulePush`, mapped onto `kls_core::belai::schedules`.

use kls_core::belai::schedules::HostSchedule;
use kls_proto::v1 as pb;

pub fn to_pb(s: &HostSchedule) -> pb::ScheduleItem {
    pb::ScheduleItem {
        id: s.id.clone(),
        profile: s.profile.clone(),
        crew: s.crew.clone(),
        cron: s.cron.clone(),
        tz: s.tz.clone(),
        cwd: s.cwd.clone(),
        enabled: s.enabled,
        last_fired_ms: s.last_fired,
        last_status: s.last_status.clone(),
        version: s.version,
        deleted: s.deleted,
        meta_json: s.meta_json.clone(),
        created_at_ms: s.created_at,
        updated_at_ms: s.updated_at,
        next_run_ms: s.next_run,
    }
}

pub fn of_pb(node_id: &str, s: pb::ScheduleItem) -> HostSchedule {
    HostSchedule {
        node_id: node_id.into(),
        id: s.id,
        profile: s.profile,
        crew: s.crew,
        cron: s.cron,
        tz: s.tz,
        cwd: s.cwd,
        enabled: s.enabled,
        last_fired: s.last_fired_ms,
        last_status: s.last_status,
        next_run: s.next_run_ms,
        version: s.version,
        deleted: s.deleted,
        meta_json: s.meta_json,
        created_at: s.created_at_ms,
        updated_at: s.updated_at_ms,
        changed_at: 0,
    }
}
