SELECT
  JSON_GROUP_ARRAY(JSON_ARRAY(FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt)) AS revision
FROM
  (
    SELECT FileName, CRC, LastProcessedLine, LastProcessedByte, Status, UpdatedAt
    FROM
      "0_import_progress"
    ORDER BY
      FileName
  )
