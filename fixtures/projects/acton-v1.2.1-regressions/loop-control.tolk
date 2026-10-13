fun flow(count: int, flag: int): int {
    var total = 0;
    repeat (count) {
        while (flag > 0) {
            match (flag) {
                1 => { flag -= 1; continue; }
                else => break,
            }
        }
        if (flag == 0) { continue; }
        total += 1;
        break;
    }
    return total;
}

fun exitEarly(flag: int): int {
    var x = 0;
    while (true) {
        if (flag == 1) { x = 4; break; }
        throw 100;
        x = flag;
        break;
    }
    return x;
}
