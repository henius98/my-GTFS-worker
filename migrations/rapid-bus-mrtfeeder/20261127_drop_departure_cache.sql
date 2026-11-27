-- Migration: Drop unused departure_cache table after removing /departures endpoint.
DROP TABLE IF EXISTS departure_cache;
