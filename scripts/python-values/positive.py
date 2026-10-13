"""Run and type-check this file only against an installed wheel outside the repo."""
import datetime as dt
import json
from pathlib import Path
from typing import assert_type

import rusty_haystack as rh
from rusty_haystack import Buf, CodecError, Coord, Float, HDateTime, HDict, HGrid, HList, Int, Nominal, NoneValue, Number
from rusty_haystack.auth import generate_nonce
from rusty_haystack.client import AsyncHaystackClient, HaystackClient, TlsConfig
from rusty_haystack.codecs import decode_typed_value, encode_typed_value
from rusty_haystack.data import HCol
from rusty_haystack.filter import CmpOp, Filter, Path as FilterPath
from rusty_haystack.graph import DiffOp, EntityGraph, GraphDiff, SharedGraph
from rusty_haystack.kinds import Int as KindInt
from rusty_haystack.ontology import DefNamespace, Slot, Spec
from rusty_haystack.server import AuthManager, HaystackServer, HisStore
from rusty_haystack.units import compatible

assert KindInt is Int and rh.kinds.Int is Int
assert issubclass(CodecError, rh.HaystackError)
assert all(value is not None for value in (generate_nonce, HaystackClient, TlsConfig, HCol, CmpOp, Filter, FilterPath, DiffOp, GraphDiff, DefNamespace, Slot, Spec, AuthManager, HaystackServer, HisStore, compatible))
number = Number(val=42, unit="kW")
assert_type(number.val, float)
assert_type(number.unit, str | None)
assert_type(Int(2**63 - 1).value, int)
assert_type(Float.from_bits(0x7FF8000000000001).bits, int)
assert_type(Buf(b"\x00\xff").data, bytes)
assert_type(Coord(1, 2).lat, float)
assert_type(Nominal("example::Serial", "A", "r1", "00042").schema_id, str)
assert_type(HDateTime(2024, 1, 1, 0, 0, 0, 3600, "opaque").dt(), dt.datetime)
maybe_number = HDict({"v": number}).get("v")
if isinstance(maybe_number, Number):
    assert_type(maybe_number.val, float)
values = HList([True, 42, 1.5, number, Int(2**63 - 1), Float.from_bits(1), NoneValue(), None, Buf(b"x"), dt.date(2024, 1, 1), dt.time(1, 2)])
entity = HDict({"id": rh.Ref("fixture"), "values": values})
native_dict: dict[str, int] = {"v": 42}
native_list: list[int] = [42]
rich_list: list[Int] = [Int(42)]
assert HDict(native_dict)["v"] == Number(42)
assert HDict.from_dict(native_dict)["v"] == Number(42)
assert HList(native_list)[0] == Number(42)
assert HList(rich_list)[0] == Int(42)
assert HDict(entity.to_dict()) == entity
assert HDict.from_dict(entity.to_dict()) == entity
assert list(HList(entity.values())) == entity.values()
wire = encode_typed_value(entity)
decoded = decode_typed_value(wire)
assert isinstance(decoded, HDict) and decoded == entity
for graph in (EntityGraph(), SharedGraph()):
    graph.add(entity)
    assert graph.get("fixture") == entity
    assert graph.remove("fixture") == entity
fixture = json.loads(Path(__file__).with_name("typed-v1-entity.json").read_text())
assert json.loads(encode_typed_value(decode_typed_value(json.dumps(fixture)))) == fixture
try:
    rh.encode_grid("text/zinc", HGrid.from_parts(HDict(), [], [entity]))
except CodecError:
    pass
else:
    raise AssertionError("rich values were silently projected by an H4 codec")

# The installed async surface is Future-valued, including context management.
# The network consumer below is a separate executable fixture in this gate.
import asyncio

assert AsyncHaystackClient is rh.AsyncHaystackClient
async def async_surface(client: AsyncHaystackClient) -> None:
    connected = client.connect(deadline=asyncio.get_running_loop().time() + 2)
    assert_type(connected, asyncio.Future[None])
    await connected
    read = client.read("site", limit=1)
    assert_type(read, asyncio.Future[HGrid])
    assert_type(await asyncio.ensure_future(read), HGrid)
    assert_type(client.close(), asyncio.Future[None])
    async with rh.AsyncHaystackClient("http://localhost:1", "user", "password") as owned:
        assert_type(owned, AsyncHaystackClient)

async def close_unconnected_owner() -> None:
    client = rh.AsyncHaystackClient("http://localhost:1", "user", "password")
    await client.close()
    assert client.closed

asyncio.run(close_unconnected_owner())
