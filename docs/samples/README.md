# Sample workbooks

`analyst-sample.xlsx` opens in any spreadsheet that reads XLSX. It holds one sheet,
**Orders**: four orders (A–C), three customers with their regions (E–F), each order's region
by `XLOOKUP` (D), the West total by `SUMIFS` (B7: 175.5) and the average order with `LET` (B8:
63.875).

It is generated from code, so it is reproducible:

```sh
cargo test -p alephgrid-core --lib write_sample_workbook -- --ignored
```

and `the_published_sample_workbook_opens_with_its_documented_values` checks on every test run
that the committed file opens with exactly the generator's inputs and the documented values.
