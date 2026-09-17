fun first(value: int): int { return value; }
fun second(value: int): int { return value + 1; }

fun flow(mutate x: int) {
    match (x) {
        0 => while (x < 2) { x += 1; }
        else => if (x > 0) { return; } else { throw 123; }
    }
}

fun matched(x: int, flag: bool): int {
    var action = first;
    match (x) {
        0 => while (flag) { action = second; break; }
        else => if (flag) { action = second; } else { action = first; }
    }
    return action(x);
}

fun insideArm(x: int) {
    match (x) {
        0 => while (x < 2) { second(x); break; }
        else => if (x > 0) { first(x); }
    }
}
