# Cjindex CityJSONFeature Alias Example

This fixture is a minimal extract of the real `amsup-subset` input shape that
triggers duplicate CityObject IDs downstream in Tyler.

The source contains one physical `CityJSONFeature` package with two
`CityObjects`:

- `NL.IMBAG.Pand.0363100012080150`
- `NL.IMBAG.Pand.0363100012080150-0`

Current `cityjson-index` indexes both CityObject keys as separate feature rows,
but both rows point to the same source byte range and reconstruct the same full
package. This creates two logical refs for one physical package.
