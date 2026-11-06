//! LET / LAMBDA 函数值：把函数当参数传、当返回值用
use sheetcore::formula::{parse, Calc, FValue};
use sheetcore::model::Workbook;

fn eval(f: &str) -> FValue {
    let wb = Workbook::new_blank();
    let node = parse(f).unwrap_or_else(|e| panic!("解析失败 {f}: {e:?}"));
    let mut c = Calc::new(&wb);
    c.eval_node(&node, 0)
}

#[test]
fn let_binds_lambda_and_calls_it() {
    assert_eq!(eval("LET(f,LAMBDA(x,x+1),f(1))"), FValue::Number(2.0));
    assert_eq!(eval("LET(f,LAMBDA(x,x*2),f(21))"), FValue::Number(42.0));
    // 两个参数
    assert_eq!(eval("LET(add,LAMBDA(a,b,a+b),add(3,4))"), FValue::Number(7.0));
    // 闭包捕获外层 LET 变量
    assert_eq!(eval("LET(n,10,f,LAMBDA(x,x+n),f(5))"), FValue::Number(15.0));
}

#[test]
fn nested_lambdas() {
    // 返回函数的函数：outer(2)(3) = 2+3
    assert_eq!(
        eval("LET(mk,LAMBDA(a,LAMBDA(b,a+b)),LET(f,mk(2),f(3)))"),
        FValue::Number(5.0)
    );
    // 函数作为参数传给另一个函数
    assert_eq!(
        eval("LET(apply,LAMBDA(g,v,g(v)),apply(LAMBDA(x,x*x),9))"),
        FValue::Number(81.0)
    );
}

#[test]
fn lambda_still_works_with_map_and_reduce() {
    assert_eq!(eval("SUM(MAP({1,2,3},LAMBDA(x,x*2)))"), FValue::Number(12.0));
    assert_eq!(eval("REDUCE(0,{1,2,3},LAMBDA(a,b,a+b))"), FValue::Number(6.0));
}

#[test]
fn bare_lambda_displays_as_error() {
    // 直接算一个没被调用的 LAMBDA：Excel 显示 #CALC!
    let v = eval("LAMBDA(x,x+1)");
    assert!(matches!(v, FValue::Lambda(_)), "应是一等公民函数值: {v:?}");
    assert_eq!(v.display(), "#FUNC!");
}

#[test]
fn missing_argument_is_omitted() {
    // ISOMITTED 判断缺省参数
    assert_eq!(
        eval("LET(f,LAMBDA(a,b,IF(ISOMITTED(b),-1,b)),f(1))"),
        FValue::Number(-1.0)
    );
    assert_eq!(
        eval("LET(f,LAMBDA(a,b,IF(ISOMITTED(b),-1,b)),f(1,7))"),
        FValue::Number(7.0)
    );
}
