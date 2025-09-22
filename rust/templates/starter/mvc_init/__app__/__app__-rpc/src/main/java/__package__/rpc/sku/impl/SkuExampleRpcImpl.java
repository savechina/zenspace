package __package__.rpc.sku.impl;

import __package__.rpc.sku.SkuExampleRpc;
import __package__.rpc.sku.dto.SkuExampleDTO;
import org.springframework.stereotype.Service;

import java.util.List;

/**
 * SkuExampleRpcImpl
 *
 * @author weirenyan
 * @date 2023/12/6
 */
@Service
public class SkuExampleRpcImpl implements SkuExampleRpc {

    /**
     * 根据skuid查询sku主数据信息(支持批量)
     *
     * @param skuIds
     * @return
     */
    @Override
    public List<SkuExampleDTO> fetchSkuList(List<String> skuIds) {
        return null;
    }
}
