"""Each marked line must produce a real static error in the installed consumer."""
import rusty_haystack as rh

number = rh.Number(1)
coord = rh.Coord(0, 0)
number.val = 2.0  # reject
number.unit = "kW"  # reject
coord.lat = 1.0  # reject
rh.Int(1).value = 2  # reject
rh.Float(1.0).bits = 0  # reject
rh.Buf(b"x").data = b"y"  # reject
rh.Nominal("a::B", "a", "r", "x").value = "y"  # reject
rh.Number("bad")  # reject
rh.Coord("bad", 0)  # reject
rh.Int("bad")  # reject
rh.Float.from_bits("bad")  # reject
rh.Buf([1, 2])  # reject
rh.HDict({"bad": object()})  # reject
rh.HList([object()])  # reject
rh.encode_typed_value(object())  # reject
rh.HaystackClient  # reject: lives only in the client submodule
