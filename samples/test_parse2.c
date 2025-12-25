struct Point {
    int x;
    int y;
};

typedef unsigned long size_t;

enum Color { RED, GREEN, BLUE };

int add(int a, int b) {
    return a + b;
}

int main(void) {
    struct Point p;
    p.x = 10;
    p.y = 20;

    if (p.x > 0) {
        p.x = -p.x;
    }

    for (int i = 0; i < 10; i++) {
        p.x += i;
    }

    return add(p.x, p.y);
}
