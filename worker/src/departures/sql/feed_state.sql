SELECT
  JSON_GROUP_ARRAY(JSON_ARRAY(Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt)) AS revision,
  COALESCE(MAX(Status IS NOT 0), 0) AS importing
FROM
  (
    SELECT Provider, FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt
    FROM
      import_progress
    ORDER BY
      Provider,
      FileName
  )
