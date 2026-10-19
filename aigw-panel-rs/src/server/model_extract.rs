//! 通用模型清单解析 —— 移植自 Python `app/model_extract.extract_generic`。
//!
//! 各平台「模型清单」接口的响应形状五花八门，这里做**统一提取**：
//!   · gwextra 包装：`{"platform":..,"action":..,"data": <真响应>}`
//!   · OpenAI 官方：`{"object":"list","data":[{"id":..}]}`
//!   · 库库：`{"data":{"model_list":[{"model_name":..,"display_name":..}]}}`
//!   · 元宝：`{"modelList":[{"chatModelId":..,"modelName":..,"modelDesc":..}]}`
//!   · WPS 灵犀：模型项是 `{key,name,desc,tag}`（id 叫 key）
//!   · Lobster：`data[].{modelId,modelName,...}`（驼峰）
//!
//! 关键点（照抄 Python 踩过的坑）：
//!   · `data` 只在**确实是 dict/list** 时才下钻，否则 `data:null` 会把真响应当空；
//!   · id 取键顺序：model_name → chatModelId → model_id → modelId → model → id → value → key
//!     （库库/元宝的 `id` 不是对话标识；WPS 的 `key` 放链尾兜底，避免误伤正常带 id 的平台）。

use serde_json::{json, Value};

const ARRAY_KEYS: [&str; 5] = ["models", "model_list", "modelList", "list", "items"];

pub fn extract_generic(resp: &Value) -> Vec<Value> {
    // data 只有是容器时才下钻
    let mut data: &Value = match resp.get("data") {
        Some(d) if d.is_object() || d.is_array() => d,
        _ => resp,
    };
    // 逐层下钻（最多 3 层）：当前层没有模型数组键、且带 data 容器 → 往下走
    for _ in 0..3 {
        let drill = data.is_object()
            && !ARRAY_KEYS.iter().any(|k| data.get(k).is_some())
            && matches!(data.get("data"), Some(Value::Object(_)) | Some(Value::Array(_)));
        if drill {
            if let Some(d) = data.get("data") {
                data = d;
            }
        } else {
            break;
        }
    }
    // 命中模型数组键
    if data.is_object() {
        for k in ARRAY_KEYS {
            if matches!(data.get(k), Some(Value::Array(_))) {
                if let Some(d) = data.get(k) {
                    data = d;
                }
                break;
            }
        }
    }
    let Some(arr) = data.as_array() else {
        return Vec::new();
    };

    let mut out: Vec<Value> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for x in arr {
        let Some(o) = x.as_object() else { continue };
        let pick = |keys: &[&str]| -> Option<String> {
            for k in keys {
                if let Some(v) = o.get(*k) {
                    match v {
                        Value::String(s) if !s.is_empty() => return Some(s.clone()),
                        Value::Number(n) => return Some(n.to_string()),
                        _ => {}
                    }
                }
            }
            None
        };
        let Some(mid) = pick(&[
            "model_name",
            "chatModelId",
            "model_id",
            "modelId",
            "model",
            "id",
            "value",
            "key",
        ]) else {
            continue;
        };
        if !seen.insert(mid.clone()) {
            continue;
        }
        let name = pick(&["name", "modelName", "display_name", "label", "title"])
            .unwrap_or_else(|| mid.clone());
        let desc = pick(&["desc", "modelDesc", "description"]).unwrap_or_default();
        let credits = pick(&["credits", "cost_ratio"]).unwrap_or_default();
        out.push(json!({
            "id": mid,
            "name": name,
            "key": mid,
            "desc": desc,
            "credits": credits,
            "on": true,
        }));
    }
    out
}
