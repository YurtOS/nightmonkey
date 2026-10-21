// hoist_reads lifted a loop's entry guard to the preheader with its operand
// chain but not what its failure edge passes: a loop-local constant stayed in
// the loop, so the hoisted guard used a value its block did not dominate
// ("BUG: invalid MIR"). The result is right either way (baseline took over);
// run with --strict-coverage, as the CI tier-regression step does.
function testUndefinedBooleanCmp()
{
    var t = true, f = false, x = [];
    for (var i = 0; i < 10; ++i) {
        x[0] = t == undefined;
        x[1] = t != undefined;
        x[2] = t === undefined;
        x[3] = t !== undefined;
        x[4] = t < undefined;
        x[5] = t > undefined;
        x[6] = t <= undefined;
        x[7] = t >= undefined;
        x[8] = f == undefined;
        x[9] = f != undefined;
        x[10] = f === undefined;
        x[11] = f !== undefined;
        x[12] = f < undefined;
        x[13] = f > undefined;
        x[14] = f <= undefined;
        x[15] = f >= undefined;
    }
    return x.join(",");
}
assertEq(testUndefinedBooleanCmp(), "false,true,false,true,false,false,false,false,false,true,false,true,false,false,false,false");
