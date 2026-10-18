"""Columns a table declares in the BigQuery DDL (scripts/gcp/bigquery_schema.sql).

Usage: python3 -I ddl_columns.py <ddl.sql> <table>
Prints one column name per line: those of the table's CREATE TABLE statement, then those that
an `ALTER TABLE ... ADD COLUMN IF NOT EXISTS` adds to datasets created before them. Exits 1
when the table is not declared.
"""

import re
import sys


def declared_columns(ddl: str, table: str) -> list[str]:
    create = re.search(
        r"CREATE TABLE IF NOT EXISTS `[^.`]+\." + re.escape(table) + r"` \((.*?)\n\)",
        ddl,
        re.S,
    )
    if create is None:
        raise KeyError(table)
    names = re.findall(r"^\s*([a-z_][a-z0-9_]*)\s+[A-Z]", create.group(1), re.M)
    names += re.findall(
        r"ALTER TABLE `[^.`]+\." + re.escape(table) + r"` ADD COLUMN IF NOT EXISTS ([a-z_][a-z0-9_]*)",
        ddl,
    )
    return list(dict.fromkeys(names))


def main() -> int:
    ddl_path, table = sys.argv[1], sys.argv[2]
    with open(ddl_path, encoding="utf-8") as ddl:
        try:
            columns = declared_columns(ddl.read(), table)
        except KeyError:
            print(f"table {table} is not declared in {ddl_path}", file=sys.stderr)
            return 1
    print("\n".join(columns))
    return 0


if __name__ == "__main__":
    sys.exit(main())
