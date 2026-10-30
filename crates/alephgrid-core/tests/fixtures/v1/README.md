Workbook written by Alephgrid at commit `a0ca964` (phase 2, storage format v1: dense `_rf_id`,
no `_rf_meta`), through its server API: `people` (5 rows, formula column `double`, a middle
delete and insert, then an edit), a saved view "Big spenders", a sheet "Model" with formulas,
and a 1,000-row `flights` demo table. Used by the migration tests; do not regenerate it with
newer code.

Expected `people` rows in order (id, name, amount, day, double):
1 Ada 10.5 2024-02-29 21 · 3 東京 0 2024-03-02 0 · NULL Inserted 1.25 NULL 2.5 ·
4 Grace 99.99 2024-03-03 199.98 · 5 NULL 7.1 2024-03-04 14.2
