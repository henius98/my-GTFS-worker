import csv
from pathlib import Path


def count_csv_rows(folder_path, has_header=True, sort_by="row", descending=False):
  base_path = Path(folder_path)
  results = []

  for file_path in base_path.rglob("*"):
    if file_path.is_file() and file_path.suffix.lower() == ".csv":
      rel_path = file_path.relative_to(base_path).as_posix()
      try:
        with open(file_path, mode="r", encoding="utf-8-sig", newline="") as f:
          reader = csv.reader(f)
          if has_header:
            next(reader, None)
          rows = sum(1 for _ in reader)
        results.append((rel_path, rows))
      except Exception as e:
        print(f"Skipped {rel_path} ({e})")

  if not results:
    print("No CSV files found.")
    return 0

  if sort_by == "rows":
    results.sort(key=lambda x: (-x[1] if descending else x[1], x[0]))
  elif sort_by == "path":
    results.sort(key=lambda x: x[0], reverse=descending)
  else:
    raise ValueError("sort_by must be 'path' or 'rows'")

  total_rows = sum(rows for _, rows in results)
  total_label = f"Total ({len(results)} files)"
  path_width = max(max(len(p) for p, _ in results), len(total_label))
  num_width = len(f"{total_rows:,}")

  for rel_path, rows in results:
    print(f"{rel_path:<{path_width}} : {rows:>{num_width},}")

  print("-" * (path_width + num_width + 3))
  print(f"{total_label:<{path_width}} : {total_rows:>{num_width},}")
  return total_rows


# Examples:
count_csv_rows(
  "../bruno/Original GTFS/extracted", sort_by="rows", descending=True
)  # Largest first
# count_csv_rows("../bruno/Original GTFS/extracted", sort_by="rows")  # Smallest first
