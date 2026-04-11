# Plan: block return value の不要括弧除去 (v2)

## 前回の失敗分析

Deref `(*expr)` と AddrOf `(&mut expr)` の括弧を Top で除去すると
4件のエラーが増加した。

原因: Rust では `*a.field` が `*(a.field)` に解釈されるため、
`(*a).field` → `*a.field` は意味が変わる。
Deref の内部式がフィールドアクセスの場合、括弧は構文上必須。

## 安全に括弧を除去できるケース

| 式 | Top で除去可能? | 理由 |
|----|----------------|------|
| `(*expr)` Deref | **不可** | `*a.field` ≠ `(*a).field` |
| `(&mut expr)` AddrOf | **不可** | `&mut a.field` ≠ `(&mut a).field` |
| `(if ... { } else { })` Conditional | **可能** | ブロック最終値で安全 |
| `(-expr)` Neg | **部分的** | `-a.field` は OK、`-a * b` は NG |
| `(!expr)` Not | **部分的** | 同上 |

## 結論: block return value の括弧除去は断念

Conditional `(if ... { A } else { B })` の括弧除去も危険。
`(if cond { A } else { B }) + 1` → `if cond { A } else { B + 1}` に解釈される。

Deref, AddrOf, Conditional のいずれも、ブロック最終値以外の文脈
（Binary のオペランド等）で使われる可能性があるため、
括弧を外側から除去するアプローチでは安全に対処できない。

**安全に括弧を除去するには**: ExprContext に Binary の優先順位情報を含め、
Conditional/Deref/AddrOf が「最外レベル」（後続の演算子がない）位置にあることを
保証する必要がある。しかしこれは現在のアーキテクチャでは困難。

block return value の 41 件は**現状のまま維持**。
