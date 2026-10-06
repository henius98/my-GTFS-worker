# Migrations Validation Report

## Replay-safety migrations

- ✅ Existing duplicate rows are cleaned and future replay is idempotent.

## Provider: `mybas-johor`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `areas` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `fare_leg_rules` matches exactly.
- ✅ `fare_media` matches exactly.
- ✅ `fare_products` matches exactly.
- ✅ `rider_categories` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `shapes` matches exactly.
- ✅ `stop_areas` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.

## Provider: `ktmb`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `calendar_dates` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.

## Provider: `rapid-bus-mrtfeeder`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `calendar_dates` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `shapes` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.

## Provider: `rapid-rail-kl`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `frequencies` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `shapes` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.

## Provider: `rapid-bus-kl`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `frequencies` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `shapes` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.

## Provider: `rapid-bus-penang`

- ✅ Local migration chain matches the compile-time schema and uses one progress table; identical UPSERT replay is idempotent with zero changed rows and every table satisfies the two-write reservation bound.
- ✅ `agency` matches exactly.
- ✅ `calendar` matches exactly.
- ✅ `routes` matches exactly.
- ✅ `shapes` matches exactly.
- ✅ `stop_times` matches exactly.
- ✅ `stops` matches exactly.
- ✅ `trips` matches exactly.
