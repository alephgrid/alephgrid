Workbook written by Alephgrid at commit `bbd4c2c` (phase 3, storage format v2), through its
server API (`/api/workbooks` and the WebSocket protocol), then stopped with SIGTERM:
`sales` created from a query (id BIGINT, region, amount DECIMAL(10,2)), formula column
`doubled = [amount] * 2`, one row inserted at position 1 and edited (id 9007199254740993,
region "inserted"), the row at position 3 deleted (so display order is no longer dense), a
saved view "North only" (region = north, sorted by id descending), and a sheet "Model" with
A1 = 21, B1 = =A1*2. Revision 8. Used by the migration tests; do not regenerate it with newer code.

Expected `sales` rows in order (record id: id, region, amount, doubled):
0: 1 north 10.25 20.50 · 4: 9007199254740993 inserted NULL NULL · 1: 2 south 0.10 0.20 ·
3: 4 東京 NULL NULL
