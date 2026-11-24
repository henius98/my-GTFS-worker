LEFT JOIN agency a
ON a.agency_id = COALESCE(NULLIF(r.agency_id, ''), (SELECT CASE WHEN COUNT(*) = 1 THEN MIN(agency_id) END FROM agency))
